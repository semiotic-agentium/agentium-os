//! Axum router for the repository API.

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{MethodRouter, delete, get, post, put},
};

use crate::{handlers, service::RepositoryService};

/// Build the repository API router.
///
/// Mount this under a prefix (e.g. `/repository`) in your application router.
pub fn repository_router(service: Arc<RepositoryService>) -> Router {
    let blob_put_route: MethodRouter<Arc<RepositoryService>> =
        put(handlers::put_blob).layer(DefaultBodyLimit::max(6 * 1024 * 1024));

    Router::new()
        .route("/agents", get(handlers::list_agents))
        .route("/agents/{name}/versions", get(handlers::list_versions))
        .route("/entries", get(handlers::get_entries))
        .route("/entries/{hash}", get(handlers::get_by_hash))
        .route("/entries/{name}/{version}", get(handlers::get_by_version))
        .route("/publish", post(handlers::publish))
        .route("/fork", post(handlers::fork))
        .route("/search", post(handlers::search))
        .route("/lineage/{hash}", get(handlers::get_lineage))
        .route("/blobs/{hash}", blob_put_route)
        .route("/blobs/{hash}", get(handlers::get_blob))
        .route("/entries/{hash}/tags", post(handlers::add_tag))
        .route("/entries/{hash}/tags", delete(handlers::remove_tag))
        .with_state(service)
}
