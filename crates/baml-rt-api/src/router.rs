//! Axum router and API state for the HTTP surface.

use axum::Router;
use baml_rt_a2a::AgentRegistry;
use std::path::Path;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa_axum::router::OpenApiRouter;

use crate::handlers;

/// Shared state for API handlers: registry (from runner) and OpenAPI spec.
#[derive(Clone)]
pub struct ApiState {
    pub registry: Arc<dyn AgentRegistry>,
    pub openapi: Arc<OpenApiSpec>,
}

async fn serve_openapi_json(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::Json<OpenApiSpec> {
    axum::Json(state.openapi.as_ref().clone())
}

/// Build the API router with discovery, A2A forward (POST + SSE), OpenAPI spec, and Swagger UI.
/// When `web_dir` is provided, serves static files from that directory as a fallback
/// (API routes always take priority). Unmatched paths fall back to `index.html` for SPA routing.
pub fn api_router(registry: Arc<dyn AgentRegistry>, web_dir: Option<&Path>) -> Router {
    let (api_router, openapi) = OpenApiRouter::new()
        .routes(utoipa_axum::routes!(handlers::list_agents))
        .routes(utoipa_axum::routes!(handlers::post_a2a))
        .routes(utoipa_axum::routes!(handlers::post_a2a_sse))
        .split_for_parts();

    let state = Arc::new(ApiState {
        registry,
        openapi: Arc::new(openapi),
    });

    let mut router = api_router
        .route("/openapi.json", axum::routing::get(serve_openapi_json))
        .with_state(state);

    if let Some(dir) = web_dir {
        let fallback = ServeDir::new(dir)
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(dir.join("index.html")));
        router = router.fallback_service(fallback);
    }

    router
}

/// Run the HTTP server on the given bind address.
/// When `web_dir` is provided, the server also serves static files from that directory.
pub async fn serve(
    registry: Arc<dyn AgentRegistry>,
    bind: &str,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = api_router(registry, web_dir);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, web_dir = ?web_dir, "HTTP API listening");
    axum::serve(listener, app).await?;
    Ok(())
}
