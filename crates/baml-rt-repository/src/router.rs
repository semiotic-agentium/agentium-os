// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Axum router for the repository API.

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{delete, get, post},
};

use crate::{handlers, service::RepositoryService};

/// Build the repository API router.
///
/// This crate cannot build agent packages, so it does not expose release-producing
/// `/publish` or `/fork` routes. Hosts wire those to their builder. Source-only records
/// are available under `/drafts` with a distinct [`crate::commands::DraftResult`] response.
pub fn repository_router(service: Arc<RepositoryService>) -> Router {
    repository_router_full(service)
}

/// Compatibility alias. Both router builders now omit release-producing publish/fork.
pub fn repository_router_without_publish(service: Arc<RepositoryService>) -> Router {
    repository_router_full(service)
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
        .route("/tools", get(handlers::list_tools))
        .route("/mcp/servers", get(handlers::list_mcp_servers))
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
        .route("/external-tools", get(handlers::list_external_tools))
        .route(
            "/external-tools/snapshots",
            get(handlers::list_approved_external_tool_snapshots),
        )
        .route(
            "/external-tools/snapshot",
            get(handlers::get_external_tool_snapshot),
        )
        .route(
            "/external-tools/versions",
            get(handlers::list_external_tool_versions),
        )
        .route(
            "/static-tools/snapshots",
            get(handlers::get_static_tool_catalog),
        )
        .with_state(service)
}

/// Mutation repository routes (drafts, tags).
///
/// These should be protected by operator authentication in production.
/// Release-producing publish/fork live in the host because they require a builder.
pub fn repository_mutation_router(service: Arc<RepositoryService>) -> Router {
    Router::new()
        .route("/drafts", post(handlers::publish_draft))
        .route("/drafts/fork", post(handlers::fork_draft))
        .route("/entries/{hash}/tags", post(handlers::add_tag))
        .route("/entries/{hash}/tags", delete(handlers::remove_tag))
        .route("/mcp/snapshots/import", post(handlers::import_mcp_snapshot))
        .route(
            "/mcp/servers/{server_id}/versions/{version}/mark-stale",
            post(handlers::mark_mcp_version_stale),
        )
        .route(
            "/external-tools/publish",
            post(handlers::publish_external_tool).layer(DefaultBodyLimit::max(
                baml_rt_tools::external_tools::MAX_KTOOL_BUNDLE_BYTES,
            )),
        )
        .route(
            "/external-tools/service/publish",
            post(handlers::publish_service_tool),
        )
        .route(
            "/external-tools/snapshots/import",
            post(handlers::import_external_tool_snapshot),
        )
        .route(
            "/external-tools/snapshots/mark-stale",
            post(handlers::mark_external_tool_version_stale),
        )
        .with_state(service)
}

fn repository_router_full(service: Arc<RepositoryService>) -> Router {
    let router = Router::new()
        .route("/agents", get(handlers::list_agents))
        .route("/agents/{name}/versions", get(handlers::list_versions))
        .route("/entries", get(handlers::get_entries))
        .route("/entries/{hash}", get(handlers::get_by_hash))
        .route("/entries/{name}/{version}", get(handlers::get_by_version))
        .route("/drafts", post(handlers::publish_draft))
        .route("/drafts/fork", post(handlers::fork_draft))
        .route("/search", post(handlers::search))
        .route("/lineage/{hash}", get(handlers::get_lineage))
        .route("/blobs/{hash}", get(handlers::get_blob))
        .route("/tools", get(handlers::list_tools))
        .route("/mcp/servers", get(handlers::list_mcp_servers))
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
        .route("/external-tools", get(handlers::list_external_tools))
        .route(
            "/external-tools/snapshots",
            get(handlers::list_approved_external_tool_snapshots),
        )
        .route(
            "/external-tools/snapshot",
            get(handlers::get_external_tool_snapshot),
        )
        .route(
            "/external-tools/versions",
            get(handlers::list_external_tool_versions),
        )
        .route(
            "/static-tools/snapshots",
            get(handlers::get_static_tool_catalog),
        )
        .route(
            "/external-tools/publish",
            post(handlers::publish_external_tool).layer(DefaultBodyLimit::max(
                baml_rt_tools::external_tools::MAX_KTOOL_BUNDLE_BYTES,
            )),
        )
        .route(
            "/external-tools/service/publish",
            post(handlers::publish_service_tool),
        )
        .route(
            "/external-tools/snapshots/import",
            post(handlers::import_external_tool_snapshot),
        )
        .route(
            "/external-tools/snapshots/mark-stale",
            post(handlers::mark_external_tool_version_stale),
        )
        .route("/entries/{hash}/tags", post(handlers::add_tag))
        .route("/entries/{hash}/tags", delete(handlers::remove_tag));

    router.with_state(service)
}
