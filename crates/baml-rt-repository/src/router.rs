//! Axum router for the repository API.

use std::sync::Arc;

use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::{handlers, service::RepositoryService};

/// Build the full repository API router (reads + mutations + publish).
///
/// Mount this under a prefix (e.g. `/repository`) in your application router.
/// Used by crate-internal integration tests; host applications use the
/// split `repository_read_router` / `repository_mutation_router` for
/// per-tier auth.
pub fn repository_router(service: Arc<RepositoryService>) -> Router {
    let publish = Router::new()
        .route("/publish", post(handlers::publish))
        .with_state(service.clone());
    repository_read_router(service.clone())
        .merge(repository_mutation_router(service))
        .merge(publish)
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
        .with_state(service)
}
