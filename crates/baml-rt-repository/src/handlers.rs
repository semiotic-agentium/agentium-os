// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
        AddTagRequest, BlobPath, EntriesQuery, EntriesQueryMode, EntriesResponse,
        ExternalToolNameQuery, ExternalToolSnapshotsResponse, ExternalToolVersionsResponse,
        ExternalToolsResponse, GetByHashPath, GetByVersionPath, HttpResult,
        ImportExternalToolSnapshotRequest, ImportExternalToolSnapshotResponse,
        ImportMcpSnapshotRequest, ImportMcpSnapshotResponse, LineagePath, LineageQuery,
        LineageResponse, ListAgentsResponse, ListVersionsPath, ListVersionsResponse, McpServerPath,
        McpServerVersionPath, McpServerVersionsResponse, McpServersResponse, McpToolLookupResponse,
        McpToolQuery, RemoveTagRequest, SearchResponse, TagPath,
    },
    ids::Version,
    search::SearchQuery,
    service::RepositoryService,
};

/// Shared state for all repository handlers.
pub type RepoState = Arc<RepositoryService>;

/// Get a repository entry by content hash (GET /repository/entries/{hash}).
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

/// List repository entries with optional name/version filter (GET /repository/entries).
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

/// Get a repository entry by agent name and version (GET /repository/entries/{name}/{version}).
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

/// List agent names in the repository (GET /repository/agents).
pub async fn list_agents(State(svc): State<RepoState>) -> HttpResult<ListAgentsResponse> {
    let agents = svc.list_agents().await.map_err(HttpApiProblem::from)?;
    Ok(Json(ListAgentsResponse { agents }))
}

/// List versions for an agent (GET /repository/agents/{name}/versions).
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

/// Fork an existing entry to create a new lineage branch (POST /repository/fork, operator-authenticated).
pub async fn fork(
    State(svc): State<RepoState>,
    Json(cmd): Json<ForkCommand>,
) -> HttpResult<PublishResult> {
    let result = svc.fork(cmd).await.map_err(HttpApiProblem::from)?;
    Ok(Json(result))
}

/// Search repository entries by metadata and content (POST /repository/search).
pub async fn search(
    State(svc): State<RepoState>,
    Json(query): Json<SearchQuery>,
) -> HttpResult<SearchResponse> {
    let results = svc.search(&query).await.map_err(HttpApiProblem::from)?;
    let total = results.len();
    Ok(Json(SearchResponse { results, total }))
}

/// Get lineage subgraph for an entry (GET /repository/lineage/{hash}).
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

/// Add a tag to a repository entry (POST /repository/entries/{hash}/tags, operator-authenticated).
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

/// Remove a tag from a repository entry (DELETE /repository/entries/{hash}/tags, operator-authenticated).
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

/// List MCP servers (GET /repository/mcp/servers).
pub async fn list_mcp_servers(State(svc): State<RepoState>) -> HttpResult<McpServersResponse> {
    let servers = svc.list_mcp_servers().await.map_err(HttpApiProblem::from)?;
    let total = servers.len();
    Ok(Json(McpServersResponse { servers, total }))
}

/// List MCP snapshot versions for one server (GET /repository/mcp/servers/{server_id}/versions).
pub async fn list_mcp_server_versions(
    State(svc): State<RepoState>,
    Path(p): Path<McpServerPath>,
) -> HttpResult<McpServerVersionsResponse> {
    let versions = svc
        .list_mcp_server_versions(&p.server_id)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(McpServerVersionsResponse {
        server_id: p.server_id,
        versions,
    }))
}

/// Get an MCP snapshot by server/version (GET /repository/mcp/servers/{server_id}/versions/{version}).
pub async fn get_mcp_snapshot(
    State(svc): State<RepoState>,
    Path(p): Path<McpServerVersionPath>,
) -> HttpResult<baml_rt_tools::mcp_snapshot::McpServerSnapshot> {
    let snapshot = svc
        .get_mcp_snapshot(&p.server_id, p.version)
        .await
        .map_err(HttpApiProblem::from)?;
    match snapshot {
        Some(snapshot) => Ok(Json(snapshot)),
        None => Err(not_found(format!(
            "MCP snapshot not found: {}@{}",
            p.server_id, p.version
        ))),
    }
}

/// Get the latest MCP snapshot for a server (GET /repository/mcp/servers/{server_id}).
pub async fn get_latest_mcp_snapshot(
    State(svc): State<RepoState>,
    Path(p): Path<McpServerPath>,
) -> HttpResult<baml_rt_tools::mcp_snapshot::McpServerSnapshot> {
    let snapshot = svc
        .get_latest_mcp_snapshot(&p.server_id)
        .await
        .map_err(HttpApiProblem::from)?;
    match snapshot {
        Some(snapshot) => Ok(Json(snapshot)),
        None => Err(not_found(format!(
            "MCP snapshot not found: {}",
            p.server_id
        ))),
    }
}

/// Find MCP tool versions by platform tool name (GET /repository/mcp/tools?platform_tool_name=...).
pub async fn find_mcp_tool(
    State(svc): State<RepoState>,
    Query(q): Query<McpToolQuery>,
) -> HttpResult<McpToolLookupResponse> {
    if q.platform_tool_name.trim().is_empty() {
        return Err(bad_request("platform_tool_name must not be empty"));
    }
    let tools = svc
        .find_mcp_tool(&q.platform_tool_name)
        .await
        .map_err(HttpApiProblem::from)?;
    let total = tools.len();
    Ok(Json(McpToolLookupResponse { tools, total }))
}

/// Import a full MCP snapshot as a new registry version (POST /repository/mcp/snapshots/import).
pub async fn import_mcp_snapshot(
    State(svc): State<RepoState>,
    Json(body): Json<ImportMcpSnapshotRequest>,
) -> HttpResult<ImportMcpSnapshotResponse> {
    let version = svc
        .put_mcp_snapshot(&body.snapshot)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(ImportMcpSnapshotResponse { version }))
}

/// Mark an MCP snapshot version stale (POST /repository/mcp/servers/{server_id}/versions/{version}/mark-stale).
pub async fn mark_mcp_version_stale(
    State(svc): State<RepoState>,
    Path(p): Path<McpServerVersionPath>,
) -> HttpResult<()> {
    svc.mark_mcp_version_stale(&p.server_id, p.version)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(()))
}

/// List external tools in the registry (GET /repository/external-tools).
pub async fn list_external_tools(
    State(svc): State<RepoState>,
) -> HttpResult<ExternalToolsResponse> {
    let tools = svc
        .list_external_tools()
        .await
        .map_err(HttpApiProblem::from)?;
    let total = tools.len();
    Ok(Json(ExternalToolsResponse { tools, total }))
}

/// List all latest-approved external-tool snapshots (GET /repository/external-tools/snapshots).
///
/// This is the builder catalog source.
pub async fn list_approved_external_tool_snapshots(
    State(svc): State<RepoState>,
) -> HttpResult<ExternalToolSnapshotsResponse> {
    let snapshots = svc
        .list_approved_external_tool_snapshots()
        .await
        .map_err(HttpApiProblem::from)?;
    let total = snapshots.len();
    Ok(Json(ExternalToolSnapshotsResponse { snapshots, total }))
}

/// List versions for one external tool (GET /repository/external-tools/versions?tool_name=...).
pub async fn list_external_tool_versions(
    State(svc): State<RepoState>,
    Query(q): Query<ExternalToolNameQuery>,
) -> HttpResult<ExternalToolVersionsResponse> {
    if q.tool_name.trim().is_empty() {
        return Err(bad_request("tool_name must not be empty"));
    }
    let versions = svc
        .list_external_tool_versions(&q.tool_name)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(ExternalToolVersionsResponse {
        tool_name: q.tool_name,
        versions,
    }))
}

/// Get an external-tool snapshot (GET /repository/external-tools/snapshot?tool_name=...&version=N).
///
/// Without `version`, returns the latest approved snapshot.
pub async fn get_external_tool_snapshot(
    State(svc): State<RepoState>,
    Query(q): Query<ExternalToolNameQuery>,
) -> HttpResult<baml_rt_tools::external_tools::ExternalToolSnapshot> {
    if q.tool_name.trim().is_empty() {
        return Err(bad_request("tool_name must not be empty"));
    }
    let snapshot = match q.version {
        Some(version) => svc
            .get_external_tool_snapshot(&q.tool_name, version)
            .await
            .map_err(HttpApiProblem::from)?,
        None => svc
            .get_latest_external_tool_snapshot(&q.tool_name)
            .await
            .map_err(HttpApiProblem::from)?,
    };
    match snapshot {
        Some(snapshot) => Ok(Json(snapshot)),
        None => Err(not_found(format!(
            "external tool snapshot not found: {}",
            q.tool_name
        ))),
    }
}

/// Import a full external-tool snapshot as a new registry version
/// (POST /repository/external-tools/snapshots/import).
///
/// Validates digest integrity, tool name, and source at the boundary so a
/// tampered snapshot is rejected here rather than failing later in the builder.
pub async fn import_external_tool_snapshot(
    State(svc): State<RepoState>,
    Json(body): Json<ImportExternalToolSnapshotRequest>,
) -> HttpResult<ImportExternalToolSnapshotResponse> {
    let snapshot = &body.snapshot;
    if snapshot.source != baml_rt_tools::external_tools::EXTERNAL_TOOL_SOURCE {
        return Err(bad_request(format!(
            "unexpected snapshot source '{}', expected '{}'",
            snapshot.source,
            baml_rt_tools::external_tools::EXTERNAL_TOOL_SOURCE
        )));
    }
    baml_rt_tools::ToolName::parse(&snapshot.tool.name)
        .map_err(|e| bad_request(format!("invalid tool name '{}': {e}", snapshot.tool.name)))?;
    baml_rt_tools::external_tools::validate_external_tool_snapshot(snapshot)
        .map_err(|e| bad_request(format!("invalid external tool snapshot: {e}")))?;
    let version = svc
        .put_external_tool_snapshot(snapshot)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(ImportExternalToolSnapshotResponse { version }))
}

/// Mark an external-tool snapshot version stale
/// (POST /repository/external-tools/snapshots/mark-stale?tool_name=...&version=N).
pub async fn mark_external_tool_version_stale(
    State(svc): State<RepoState>,
    Query(q): Query<ExternalToolNameQuery>,
) -> HttpResult<()> {
    let Some(version) = q.version else {
        return Err(bad_request("version query parameter is required"));
    };
    if q.tool_name.trim().is_empty() {
        return Err(bad_request("tool_name must not be empty"));
    }
    svc.mark_external_tool_version_stale(&q.tool_name, version)
        .await
        .map_err(HttpApiProblem::from)?;
    Ok(Json(()))
}

/// Download the built artifact blob for an entry (GET /repository/blobs/{hash}).
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
