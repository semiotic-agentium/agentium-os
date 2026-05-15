//! Axum router for the repository API.

use std::sync::Arc;

use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::{handlers, service::RepositoryService};

/// Build the repository API router.
///
/// Mount this under a prefix (e.g. `/repository`) in your application router.
pub fn repository_router(service: Arc<RepositoryService>) -> Router {
    repository_router_with_publish(service, true)
}

/// Build repository API router without the `/publish` route.
///
/// Intended for host applications that orchestrate publish externally while
/// reusing all other repository endpoints.
pub fn repository_router_without_publish(service: Arc<RepositoryService>) -> Router {
    repository_router_with_publish(service, false)
}

/// Read-only repository routes (agents, versions, entries, lineage, blobs, search).
///
/// These are safe for unauthenticated access: agents and CLI tools need to
/// discover and pull packages without operator credentials.
pub fn repository_read_router(service: Arc<RepositoryService>) -> Router {
    Router::new()
        .route("/agents", get(handlers::list_agents))
        .route("/agents/{name}/versions", get(handlers::list_versions))
        .route("/entries", get(handlers::get_entries))
        .route("/entries/{hash}", get(handlers::get_by_hash))
        .route("/entries/{name}/{version}", get(handlers::get_by_version))
        .route("/search", post(handlers::search))
        .route("/lineage/{hash}", get(handlers::get_lineage))
        .route("/blobs/{hash}", get(handlers::get_blob))
        .route(
            "/mcp/servers/{server_id}",
            get(handlers::get_latest_mcp_snapshot),
        )
        .route(
            "/mcp/servers/{server_id}/versions",
            get(handlers::list_mcp_server_versions),
        )
        .route(
            "/mcp/servers/{server_id}/versions/{version}",
            get(handlers::get_mcp_snapshot),
        )
        .route("/mcp/tools", get(handlers::find_mcp_tool))
        .with_state(service)
}

/// Mutation repository routes (fork, tags).
///
/// These should be protected by operator authentication in production.
/// Does not include `/publish` — that is wired separately by the host
/// application because publish involves a build step outside this crate.
pub fn repository_mutation_router(service: Arc<RepositoryService>) -> Router {
    Router::new()
        .route("/fork", post(handlers::fork))
        .route("/entries/{hash}/tags", post(handlers::add_tag))
        .route("/entries/{hash}/tags", delete(handlers::remove_tag))
        .route("/mcp/snapshots/import", post(handlers::import_mcp_snapshot))
        .route(
            "/mcp/servers/{server_id}/versions/{version}/mark-stale",
            post(handlers::mark_mcp_version_stale),
        )
        .with_state(service)
}

fn repository_router_with_publish(
    service: Arc<RepositoryService>,
    include_publish: bool,
) -> Router {
    let mut router = Router::new()
        .route("/agents", get(handlers::list_agents))
        .route("/agents/{name}/versions", get(handlers::list_versions))
        .route("/entries", get(handlers::get_entries))
        .route("/entries/{hash}", get(handlers::get_by_hash))
        .route("/entries/{name}/{version}", get(handlers::get_by_version))
        .route("/fork", post(handlers::fork))
        .route("/search", post(handlers::search))
        .route("/lineage/{hash}", get(handlers::get_lineage))
        .route("/blobs/{hash}", get(handlers::get_blob))
        .route(
            "/mcp/servers/{server_id}",
            get(handlers::get_latest_mcp_snapshot),
        )
        .route(
            "/mcp/servers/{server_id}/versions",
            get(handlers::list_mcp_server_versions),
        )
        .route(
            "/mcp/servers/{server_id}/versions/{version}",
            get(handlers::get_mcp_snapshot),
        )
        .route("/mcp/tools", get(handlers::find_mcp_tool))
        .route("/mcp/snapshots/import", post(handlers::import_mcp_snapshot))
        .route(
            "/mcp/servers/{server_id}/versions/{version}/mark-stale",
            post(handlers::mark_mcp_version_stale),
        )
        .route("/entries/{hash}/tags", post(handlers::add_tag))
        .route("/entries/{hash}/tags", delete(handlers::remove_tag));

    if include_publish {
        router = router.route("/publish", post(handlers::publish));
    }

    router.with_state(service)
}
