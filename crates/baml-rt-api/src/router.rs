//! Axum router and API state for the HTTP surface.

use std::{path::Path, sync::Arc};

use axum::{Router, extract::MatchedPath, http::Request};
use baml_rt_a2a::AgentRegistry;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa_axum::router::OpenApiRouter;

use crate::{ContextMetricsService, MermaidService, handlers};

/// Shared state for API handlers: registry (from runner), OpenAPI spec, optional Mermaid service.
#[derive(Clone)]
pub struct ApiState {
    pub registry: Arc<dyn AgentRegistry>,
    pub openapi: Arc<OpenApiSpec>,
    pub mermaid: Option<Arc<dyn MermaidService>>,
    pub context_metrics: Option<Arc<dyn ContextMetricsService>>,
}

async fn serve_openapi_json(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::Json<OpenApiSpec> {
    axum::Json(state.openapi.as_ref().clone())
}

/// Build the API router with discovery, A2A forward (POST + SSE), optional Mermaid, OpenAPI spec, and Swagger UI.
/// When `web_dir` is provided, serves static files from that directory as a fallback
/// (API routes always take priority). Unmatched paths fall back to `index.html` for SPA routing.
pub fn api_router(
    registry: Arc<dyn AgentRegistry>,
    mermaid: Option<Arc<dyn MermaidService>>,
    web_dir: Option<&Path>,
) -> Router {
    api_router_with_services(registry, mermaid, None, web_dir)
}

/// Build the API router with optional Mermaid and optional context metrics services.
pub fn api_router_with_services(
    registry: Arc<dyn AgentRegistry>,
    mermaid: Option<Arc<dyn MermaidService>>,
    context_metrics: Option<Arc<dyn ContextMetricsService>>,
    web_dir: Option<&Path>,
) -> Router {
    // Route-level tracing layer to capture HTTP semantic fields (including matched route template).
    let http_trace_layer = TraceLayer::new_for_http().make_span_with(|req: &Request<_>| {
        let route = req
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str)
            .unwrap_or("<unmatched>");
        tracing::info_span!(
            "baml_rt_api.http.request",
            http.request.method = %req.method(),
            http.route = %route,
            url.path = %req.uri().path(),
            span.kind = %"server",
        )
    });

    let (api_router, openapi) = OpenApiRouter::new()
        .routes(utoipa_axum::routes!(handlers::list_agents))
        .routes(utoipa_axum::routes!(handlers::post_a2a))
        .routes(utoipa_axum::routes!(handlers::post_a2a_sse))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_context))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_task))
        .routes(utoipa_axum::routes!(handlers::get_context_metrics))
        .split_for_parts();

    let mut openapi = openapi;
    let mut tag_agents = utoipa::openapi::Tag::new("agents");
    tag_agents.description = Some("Agent discovery and A2A JSON-RPC".to_string());
    let mut tag_mermaid = utoipa::openapi::Tag::new("mermaid");
    tag_mermaid.description = Some(
        "Provenance graph as Mermaid sequence diagrams (when GraphQLite is enabled)".to_string(),
    );
    let mut tag_provenance = utoipa::openapi::Tag::new("provenance");
    tag_provenance.description =
        Some("Provenance-backed context metrics (when GraphQLite is enabled)".to_string());
    openapi.tags = Some(vec![tag_agents, tag_mermaid, tag_provenance]);

    let state = Arc::new(ApiState {
        registry,
        openapi: Arc::new(openapi),
        mermaid,
        context_metrics,
    });

    let mut router = api_router
        .route("/openapi.json", axum::routing::get(serve_openapi_json))
        .route_layer(http_trace_layer)
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
    mermaid: Option<Arc<dyn MermaidService>>,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_services(registry, bind, mermaid, None, web_dir).await
}

/// Run the HTTP server with optional Mermaid and context metrics services.
pub async fn serve_with_services(
    registry: Arc<dyn AgentRegistry>,
    bind: &str,
    mermaid: Option<Arc<dyn MermaidService>>,
    context_metrics: Option<Arc<dyn ContextMetricsService>>,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = api_router_with_services(registry, mermaid, context_metrics, web_dir);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, web_dir = ?web_dir, "HTTP API listening");
    axum::serve(listener, app).await?;
    Ok(())
}
