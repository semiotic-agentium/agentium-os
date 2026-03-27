//! Axum HTTP handlers for the repository API.
//!
//! Each handler extracts path/query/body parameters, delegates to
//! `RepositoryService`, and maps errors to RFC 7807 responses.

use std::sync::Arc;

use axum::{
    extract::{Json, Path, Query, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use http_api_problem::HttpApiProblem;

use crate::{
    commands::{ForkCommand, PublishCommand, PublishResult},
    entry::{RepositoryEntry, RepositoryEntryHeader, Tag},
    http::{
        AddTagRequest, BlobPath, EntriesQuery, EntriesQueryMode, EntriesResponse, GetByHashPath,
        GetByVersionPath, HttpResult, LineagePath, LineageQuery, LineageResponse,
        ListAgentsResponse, ListVersionsPath, ListVersionsResponse, RemoveTagRequest,
        SearchResponse, TagPath,
    },
    ids::Version,
    search::SearchQuery,
    service::RepositoryService,
};

/// Shared state for all repository handlers.
pub type RepoState = Arc<RepositoryService>;

pub async fn get_by_hash(
    State(svc): State<RepoState>,
    Path(p): Path<GetByHashPath>,
) -> HttpResult<crate::entry::RepositoryEntry> {
    let hash = p.hash.parse().map_err(|e| bad_request(format!("{e}")))?;
    let entry = svc.get_by_hash(&hash).await.map_err(HttpApiProblem::from)?;
    match entry {
        Some(e) => Ok(Json(e)),
        None => Err(not_found(format!("Entry not found: {}", p.hash))),
    }
}

pub async fn get_entries(
    State(svc): State<RepoState>,
    Query(q): Query<EntriesQuery>,
) -> HttpResult<EntriesResponse> {
    let mode = EntriesQueryMode::try_from(q).map_err(bad_request)?;
    let entries = match mode {
        EntriesQueryMode::All => svc
            .search(&SearchQuery::default())
            .await
            .map_err(HttpApiProblem::from)?,
        EntriesQueryMode::ByName(name) => svc
            .list_versions(&name)
            .await
            .map_err(HttpApiProblem::from)?,
        EntriesQueryMode::ByNameVersion { name, version } => {
            match svc
                .get_by_version(&name, version)
                .await
                .map_err(HttpApiProblem::from)?
            {
                Some(entry) => vec![entry_to_header(entry)],
                None => Vec::new(),
            }
        }
    };

    let total = entries.len();
    Ok(Json(EntriesResponse { entries, total }))
}

pub async fn get_by_version(
    State(svc): State<RepoState>,
    Path(p): Path<GetByVersionPath>,
) -> HttpResult<crate::entry::RepositoryEntry> {
    let name = p.name.parse().map_err(|e| bad_request(format!("{e}")))?;
    let version: Version = p.version.parse().map_err(|e| bad_request(format!("{e}")))?;
    let entry = svc
        .get_by_version(&name, version)
        .await
        .map_err(HttpApiProblem::from)?;
    match entry {
        Some(e) => Ok(Json(e)),
        None => Err(not_found(format!(
            "Entry not found: {}@{}",
            p.name, p.version
        ))),
    }
}

pub async fn list_agents(State(svc): State<RepoState>) -> HttpResult<ListAgentsResponse> {
    let agents = svc.list_agents().await.map_err(HttpApiProblem::from)?;
    Ok(Json(ListAgentsResponse { agents }))
}

pub async fn list_versions(
    State(svc): State<RepoState>,
    Path(p): Path<ListVersionsPath>,
) -> HttpResult<ListVersionsResponse> {
    let name = p.name.parse().map_err(|e| bad_request(format!("{e}")))?;
    let versions = svc
        .list_versions(&name)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(ListVersionsResponse { name, versions }))
}

pub async fn publish(
    State(svc): State<RepoState>,
    Json(cmd): Json<PublishCommand>,
) -> HttpResult<PublishResult> {
    let result = svc.publish(cmd).await.map_err(HttpApiProblem::from)?;
    Ok(Json(result))
}

pub async fn fork(
    State(svc): State<RepoState>,
    Json(cmd): Json<ForkCommand>,
) -> HttpResult<PublishResult> {
    let result = svc.fork(cmd).await.map_err(HttpApiProblem::from)?;
    Ok(Json(result))
}

pub async fn search(
    State(svc): State<RepoState>,
    Json(query): Json<SearchQuery>,
) -> HttpResult<SearchResponse> {
    let results = svc.search(&query).await.map_err(HttpApiProblem::from)?;
    let total = results.len();
    Ok(Json(SearchResponse { results, total }))
}

pub async fn get_lineage(
    State(svc): State<RepoState>,
    Path(p): Path<LineagePath>,
    Query(q): Query<LineageQuery>,
) -> HttpResult<LineageResponse> {
    let hash = p.hash.parse().map_err(|e| bad_request(format!("{e}")))?;
    let subgraph = svc
        .lineage(&hash, q.depth)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(LineageResponse { subgraph }))
}

pub async fn add_tag(
    State(svc): State<RepoState>,
    Path(p): Path<TagPath>,
    Json(body): Json<AddTagRequest>,
) -> HttpResult<()> {
    let hash = p.hash.parse().map_err(|e| bad_request(format!("{e}")))?;
    svc.add_tag(&hash, Tag::new(body.tag))
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(()))
}

pub async fn remove_tag(
    State(svc): State<RepoState>,
    Path(p): Path<TagPath>,
    Json(body): Json<RemoveTagRequest>,
) -> HttpResult<()> {
    let hash = p.hash.parse().map_err(|e| bad_request(format!("{e}")))?;
    let tag = Tag::new(body.tag);
    svc.remove_tag(&hash, &tag)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(()))
}

pub async fn get_blob(
    State(svc): State<RepoState>,
    Path(p): Path<BlobPath>,
) -> std::result::Result<impl IntoResponse, HttpApiProblem> {
    let hash = p.hash.parse().map_err(|e| bad_request(format!("{e}")))?;
    let Some(data) = svc.get_blob(&hash).await.map_err(HttpApiProblem::from)? else {
        return Err(not_found(format!("Blob not found for hash: {}", p.hash)));
    };

    let disposition = format!("attachment; filename=\"{hash}.tar.gz\"");
    let mut response = Response::new(axum::body::Body::from(data));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

fn bad_request(detail: impl Into<String>) -> HttpApiProblem {
    HttpApiProblem::new(http_api_problem::StatusCode::BAD_REQUEST).detail(detail)
}

fn not_found(detail: String) -> HttpApiProblem {
    HttpApiProblem::new(http_api_problem::StatusCode::NOT_FOUND).detail(detail)
}

fn entry_to_header(entry: RepositoryEntry) -> RepositoryEntryHeader {
    let description = entry.source.manifest.description().map(str::to_string);
    let tools = entry
        .source
        .manifest
        .tools()
        .into_iter()
        .map(str::to_string)
        .collect();
    let capabilities = entry
        .source
        .manifest
        .capabilities()
        .into_iter()
        .map(str::to_string)
        .collect();

    RepositoryEntryHeader {
        hash: entry.hash,
        version_ref: entry.version_ref,
        parentage: entry.parentage,
        generation: entry.generation,
        change_rationale: entry.change_rationale,
        created_at: entry.created_at,
        tags: entry.tags,
        description,
        tools,
        capabilities,
    }
}
