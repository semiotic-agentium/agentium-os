//! Axum HTTP handlers for the repository API.
//!
//! Each handler extracts path/query/body parameters, delegates to
//! `RepositoryService`, and maps errors to RFC 7807 responses.

use std::sync::Arc;

use axum::extract::{Json, Path, Query, State};
use http_api_problem::HttpApiProblem;

use crate::{
    commands::{ForkCommand, PublishCommand, PublishResult},
    entry::{FitnessDomain, Tag},
    http::{
        AddTagRequest, GetByHashPath, GetByVersionPath, HttpResult, LineagePath, LineageQuery,
        LineageResponse, ListAgentsResponse, ListVersionsPath, ListVersionsResponse,
        RecordFitnessPath, RecordFitnessRequest, RemoveTagRequest, SearchResponse, TagPath,
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

pub async fn record_fitness(
    State(svc): State<RepoState>,
    Path(p): Path<RecordFitnessPath>,
    Json(body): Json<RecordFitnessRequest>,
) -> HttpResult<()> {
    let hash = p.hash.parse().map_err(|e| bad_request(format!("{e}")))?;
    let domain = FitnessDomain::new(body.domain);
    svc.record_fitness(&hash, domain, body.score)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(()))
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

fn bad_request(detail: String) -> HttpApiProblem {
    HttpApiProblem::new(http_api_problem::StatusCode::BAD_REQUEST).detail(detail)
}

fn not_found(detail: String) -> HttpApiProblem {
    HttpApiProblem::new(http_api_problem::StatusCode::NOT_FOUND).detail(detail)
}
