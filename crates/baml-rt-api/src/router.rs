//! Axum router and API state for the HTTP surface.
//!
//! Config, tool catalog, and secret resolver are **injected as required dependencies** (no Option).
//! Use `api_router()` for a minimal router with in-memory config and empty catalog/resolver;
//! use `api_router_with_services()` to inject real implementations.

use std::{path::Path, sync::Arc};

use axum::{Router, extract::MatchedPath, http::Request};
use baml_rt_a2a::AgentRegistry;
use baml_rt_config::{ConfigService, SurrealConfigStore};
use baml_rt_core::DeploymentManager;
use baml_rt_llm_config::{EmptySecretResolver, RuntimeSecretStore, SecretResolver};
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa_axum::router::OpenApiRouter;

use crate::{
    ContextMetricsService, MermaidService, PlanningService, ProvenanceOpsService, config_handlers,
    handlers, repository_publish,
};

/// Shared state for API handlers: registry, OpenAPI spec, and **injected** config/catalog/resolver.
#[derive(Clone)]
pub struct ApiState {
    pub registry: Arc<dyn AgentRegistry>,
    pub openapi: Arc<OpenApiSpec>,
    pub mermaid: Option<Arc<dyn MermaidService>>,
    pub context_metrics: Option<Arc<dyn ContextMetricsService>>,
    pub provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    pub planning: Option<Arc<dyn PlanningService>>,
    pub deployment_manager: Option<Arc<dyn DeploymentManager>>,
    pub repository_url: Option<String>,
    pub tool_catalog: Arc<dyn ToolCatalog>,
    pub config_service: Arc<dyn ConfigService>,
    pub secret_resolver: Arc<dyn SecretResolver>,
    pub runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
}

async fn serve_openapi_json(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::Json<OpenApiSpec> {
    axum::Json(state.openapi.as_ref().clone())
}

/// Build a minimal API router with default config/catalog/resolver (in-memory config, empty catalog, no-op resolver).
/// For production, use `api_router_with_services` and inject real implementations.
pub async fn api_router(
    registry: Arc<dyn AgentRegistry>,
    mermaid: Option<Arc<dyn MermaidService>>,
    web_dir: Option<&Path>,
) -> Router {
    let tool_catalog: Arc<dyn ToolCatalog> = Arc::new(InventoryCatalog::new());
    let config_service: Arc<dyn ConfigService> = Arc::new(
        SurrealConfigStore::in_memory()
            .await
            .expect("in-memory config store for API"),
    );
    let secret_resolver: Arc<dyn SecretResolver> = Arc::new(EmptySecretResolver);
    api_router_with_services(
        registry,
        mermaid,
        None,
        None,
        None,
        tool_catalog,
        config_service,
        secret_resolver,
        None,
        web_dir,
    )
}

/// Build the API router with injected dependencies (required: tool_catalog, config_service, secret_resolver).
/// When `runtime_secret_store` is Some, PUT /config/secrets/{name} provisions secrets in the UI.
#[allow(clippy::too_many_arguments)]
pub fn api_router_with_services(
    registry: Arc<dyn AgentRegistry>,
    mermaid: Option<Arc<dyn MermaidService>>,
    context_metrics: Option<Arc<dyn ContextMetricsService>>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    planning: Option<Arc<dyn PlanningService>>,
    tool_catalog: Arc<dyn ToolCatalog>,
    config_service: Arc<dyn ConfigService>,
    secret_resolver: Arc<dyn SecretResolver>,
    runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    web_dir: Option<&Path>,
) -> Router {
    api_router_with_services_and_deploy(
        registry,
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        None,
        None,
        None,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        web_dir,
    )
}

/// Build API router with optional deployment manager and repository URL wiring.
#[allow(clippy::too_many_arguments)]
pub fn api_router_with_services_and_deploy(
    registry: Arc<dyn AgentRegistry>,
    mermaid: Option<Arc<dyn MermaidService>>,
    context_metrics: Option<Arc<dyn ContextMetricsService>>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    planning: Option<Arc<dyn PlanningService>>,
    deployment_manager: Option<Arc<dyn DeploymentManager>>,
    repository_url: Option<String>,
    repository_service: Option<Arc<baml_rt_repository::RepositoryService>>,
    tool_catalog: Arc<dyn ToolCatalog>,
    config_service: Arc<dyn ConfigService>,
    secret_resolver: Arc<dyn SecretResolver>,
    runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    web_dir: Option<&Path>,
) -> Router {
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
        .routes(utoipa_axum::routes!(handlers::post_dispatch))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_context))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_task))
        .routes(utoipa_axum::routes!(handlers::get_context_metrics))
        .routes(utoipa_axum::routes!(handlers::get_context_planning))
        .routes(utoipa_axum::routes!(handlers::get_provenance_llm_calls))
        .routes(utoipa_axum::routes!(handlers::get_provenance_tool_calls))
        .routes(utoipa_axum::routes!(handlers::get_provenance_messages))
        .routes(utoipa_axum::routes!(handlers::get_provenance_aggregates))
        .routes(utoipa_axum::routes!(handlers::post_deploy))
        .routes(utoipa_axum::routes!(handlers::post_undeploy))
        .routes(utoipa_axum::routes!(handlers::get_deployments))
        .routes(utoipa_axum::routes!(config_handlers::list_secrets_overview))
        .routes(utoipa_axum::routes!(config_handlers::list_store_keys))
        .routes(utoipa_axum::routes!(config_handlers::put_secret))
        .routes(utoipa_axum::routes!(config_handlers::delete_secret))
        .routes(utoipa_axum::routes!(config_handlers::list_config))
        .routes(utoipa_axum::routes!(config_handlers::get_config))
        .routes(utoipa_axum::routes!(config_handlers::put_config))
        .routes(utoipa_axum::routes!(config_handlers::delete_config))
        .routes(utoipa_axum::routes!(config_handlers::list_config_versions))
        .routes(utoipa_axum::routes!(config_handlers::get_config_version))
        .routes(utoipa_axum::routes!(config_handlers::list_secret_requests))
        .split_for_parts();

    let mut openapi = openapi;
    let mut tag_agents = utoipa::openapi::Tag::new("agents");
    tag_agents.description =
        Some("Agent discovery, deterministic dispatch, and A2A JSON-RPC".to_string());
    let mut tag_mermaid = utoipa::openapi::Tag::new("mermaid");
    tag_mermaid.description = Some(
        "Provenance graph as Mermaid sequence diagrams (when SurrealDB provenance is enabled)"
            .to_string(),
    );
    let mut tag_provenance = utoipa::openapi::Tag::new("provenance");
    tag_provenance.description =
        Some("Provenance-backed metrics and operational query APIs.".to_string());
    let mut tag_deployments = utoipa::openapi::Tag::new("deployments");
    tag_deployments.description =
        Some("Runner-local deployment lifecycle APIs (deploy, undeploy, list).".to_string());
    let mut tag_config = utoipa::openapi::Tag::new("config");
    tag_config.description = Some(
        "Tool configuration and secret requests (schema includes config type schemas)".to_string(),
    );
    openapi.tags = Some(vec![
        tag_agents,
        tag_mermaid,
        tag_provenance,
        tag_deployments,
        tag_config,
    ]);

    let state = Arc::new(ApiState {
        registry,
        openapi: Arc::new(openapi),
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        deployment_manager,
        repository_url,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
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
    if let Some(repo_service) = repository_service {
        let publish_router = axum::Router::new()
            .route(
                "/publish",
                axum::routing::post(repository_publish::publish_with_build),
            )
            .with_state(repo_service.clone());
        let repo_router = baml_rt_repository::repository_router_without_publish(repo_service)
            .merge(publish_router);
        router = router.nest("/repository", repo_router);
    }

    router
}

/// Run the HTTP server with default config/catalog/resolver (see `api_router`).
pub async fn serve(
    registry: Arc<dyn AgentRegistry>,
    bind: &str,
    mermaid: Option<Arc<dyn MermaidService>>,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tool_catalog: Arc<dyn ToolCatalog> = Arc::new(InventoryCatalog::new());
    let config_service: Arc<dyn ConfigService> = Arc::new(
        SurrealConfigStore::in_memory()
            .await
            .expect("in-memory config store for API"),
    );
    let secret_resolver: Arc<dyn SecretResolver> = Arc::new(EmptySecretResolver);
    serve_with_services(
        registry,
        bind,
        mermaid,
        None,
        None,
        None,
        tool_catalog,
        config_service,
        secret_resolver,
        None,
        web_dir,
    )
    .await
}

/// Run the HTTP server with injected dependencies (required: tool_catalog, config_service, secret_resolver).
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_services(
    registry: Arc<dyn AgentRegistry>,
    bind: &str,
    mermaid: Option<Arc<dyn MermaidService>>,
    context_metrics: Option<Arc<dyn ContextMetricsService>>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    planning: Option<Arc<dyn PlanningService>>,
    tool_catalog: Arc<dyn ToolCatalog>,
    config_service: Arc<dyn ConfigService>,
    secret_resolver: Arc<dyn SecretResolver>,
    runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_services_and_deploy(
        registry,
        bind,
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        None,
        None,
        None,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        web_dir,
    )
    .await
}

/// Run HTTP server with optional deployment manager and repository URL wiring.
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_services_and_deploy(
    registry: Arc<dyn AgentRegistry>,
    bind: &str,
    mermaid: Option<Arc<dyn MermaidService>>,
    context_metrics: Option<Arc<dyn ContextMetricsService>>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    planning: Option<Arc<dyn PlanningService>>,
    deployment_manager: Option<Arc<dyn DeploymentManager>>,
    repository_url: Option<String>,
    repository_service: Option<Arc<baml_rt_repository::RepositoryService>>,
    tool_catalog: Arc<dyn ToolCatalog>,
    config_service: Arc<dyn ConfigService>,
    secret_resolver: Arc<dyn SecretResolver>,
    runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = api_router_with_services_and_deploy(
        registry,
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        deployment_manager,
        repository_url,
        repository_service,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        web_dir,
    );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, web_dir = ?web_dir, "HTTP API listening");
    axum::serve(listener, app).await?;
    Ok(())
}
