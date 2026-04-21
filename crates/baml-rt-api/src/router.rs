//! Axum router and API state for the HTTP surface.
//!
//! Config, tool catalog, and secret resolver are **injected as required dependencies** (no Option).
//! Use `api_router()` for a minimal router with in-memory config and empty catalog/resolver;
//! use `api_router_with_services()` to inject real implementations.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use axum::{
    Router,
    body::Body,
    extract::MatchedPath,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use baml_rt_a2a::AgentRegistry;
use baml_rt_config::{ConfigService, SurrealConfigStore};
use baml_rt_core::DeploymentManager;
use baml_rt_llm_config::{EmptySecretResolver, RuntimeSecretStore, SecretResolver};
pub use baml_rt_router::auth::ClusterMode;
use baml_rt_router::auth::{ClusterAuthConfig, ClusterAuthLayer};
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use serde_json::json;
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa_axum::router::OpenApiRouter;

use crate::{
    ContextIndexService, ContextMetricsService, ConversationHistoryService, EpisodeService,
    MermaidService, PlanningService, ProvenanceOpsService, config_handlers, handlers, metrics,
    otel_middleware, repository_publish,
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
    pub episode: Option<Arc<dyn EpisodeService>>,
    pub conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    pub conversation_history_events: Option<Arc<dyn crate::ConversationHistoryEventService>>,
    pub context_index: Option<Arc<dyn ContextIndexService>>,
    pub deployment_manager: Option<Arc<dyn DeploymentManager>>,
    pub repository_url: Option<String>,
    pub tool_catalog: Arc<dyn ToolCatalog>,
    pub config_service: Arc<dyn ConfigService>,
    pub secret_resolver: Arc<dyn SecretResolver>,
    pub runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    /// Readiness flag: set to `true` after deployment restore completes.
    pub ready: Arc<AtomicBool>,
    /// Shared secret for authenticating control-plane requests (e.g. `/control/migrate`).
    pub runner_token: Option<String>,
    /// Deployment topology: standalone (single runner) or cluster (shared SurrealDB).
    /// Controls whether control endpoints require a configured runner token.
    pub cluster_mode: ClusterMode,
}

async fn serve_openapi_json(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::Json<OpenApiSpec> {
    let start = Instant::now();
    let spec = state.openapi.as_ref().clone();
    metrics::record_request("get_openapi_json", "success", start.elapsed());
    axum::Json(spec)
}

async fn healthz() -> StatusCode {
    let start = Instant::now();
    metrics::record_request("get_healthz", "success", start.elapsed());
    StatusCode::OK
}

async fn readyz(axum::extract::State(state): axum::extract::State<Arc<ApiState>>) -> StatusCode {
    let start = Instant::now();
    let code = if state.ready.load(Ordering::Acquire) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let result = if code.is_success() {
        "success"
    } else {
        "unavailable"
    };
    metrics::record_request("get_readyz", result, start.elapsed());
    code
}

fn repository_route_metric_label(matched: &MatchedPath) -> &'static str {
    match matched.as_str() {
        "/agents" => "repository_list_agents",
        "/agents/{name}/versions" => "repository_list_versions",
        "/entries" => "repository_get_entries",
        "/entries/{hash}" => "repository_get_by_hash",
        "/entries/{name}/{version}" => "repository_get_by_version",
        "/fork" => "repository_fork",
        "/search" => "repository_search",
        "/lineage/{hash}" => "repository_get_lineage",
        "/blobs/{hash}" => "repository_get_blob",
        "/entries/{hash}/tags" => "repository_tags",
        "/publish" => "repository_publish",
        _ => "repository_unknown",
    }
}

async fn repository_http_metrics(request: Request<Body>, next: Next) -> Response {
    let start = Instant::now();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(repository_route_metric_label)
        .unwrap_or("repository_unknown");
    let response = next.run(request).await;
    let status = response.status();
    let result = if status.is_success() {
        "success"
    } else if status == StatusCode::NOT_FOUND {
        "not_found"
    } else if status.is_client_error() {
        "client_error"
    } else {
        "internal"
    };
    metrics::record_request(route, result, start.elapsed());
    response
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
        None,
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
    episode: Option<Arc<dyn EpisodeService>>,
    conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    conversation_history_events: Option<Arc<dyn crate::ConversationHistoryEventService>>,
    context_index: Option<Arc<dyn ContextIndexService>>,
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
        episode,
        conversation_history,
        conversation_history_events,
        context_index,
        None,
        None,
        None,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        Arc::new(AtomicBool::new(true)),
        None,
        ClusterMode::Standalone,
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
    episode: Option<Arc<dyn EpisodeService>>,
    conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    conversation_history_events: Option<Arc<dyn crate::ConversationHistoryEventService>>,
    context_index: Option<Arc<dyn ContextIndexService>>,
    deployment_manager: Option<Arc<dyn DeploymentManager>>,
    repository_url: Option<String>,
    repository_service: Option<Arc<baml_rt_repository::RepositoryService>>,
    tool_catalog: Arc<dyn ToolCatalog>,
    config_service: Arc<dyn ConfigService>,
    secret_resolver: Arc<dyn SecretResolver>,
    runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    ready: Arc<AtomicBool>,
    runner_token: Option<String>,
    cluster_mode: ClusterMode,
    web_dir: Option<&Path>,
) -> Router {
    let http_trace_layer =
        TraceLayer::new_for_http().make_span_with(otel_middleware::http_request_span);

    // ── Tier 1a: Public agent routes (no auth; OTEL context-extracting
    // middleware applied so forwarded A2A requests join the ingress trace).
    let (agent_router, agent_openapi) = OpenApiRouter::new()
        .routes(utoipa_axum::routes!(handlers::post_a2a))
        .routes(utoipa_axum::routes!(handlers::post_dispatch))
        .split_for_parts();
    let agent_router = agent_router.route_layer(axum::middleware::from_fn(
        otel_middleware::extract_parent_trace_context,
    ));

    // ── Tier 1b: Other public routes (no auth) — discovery, mermaid, episode,
    // conversation history, provenance. These handlers do not consume the
    // OTEL parent-context extension so they skip the middleware.
    let (other_public_router, other_openapi) = OpenApiRouter::new()
        .routes(utoipa_axum::routes!(handlers::list_agents))
        .routes(utoipa_axum::routes!(handlers::get_context_index))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_context))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_task))
        .routes(utoipa_axum::routes!(handlers::get_context_metrics))
        .routes(utoipa_axum::routes!(handlers::get_context_planning))
        .routes(utoipa_axum::routes!(handlers::get_provenance_llm_calls))
        .routes(utoipa_axum::routes!(handlers::get_provenance_tool_calls))
        .routes(utoipa_axum::routes!(handlers::get_provenance_messages))
        .routes(utoipa_axum::routes!(handlers::get_provenance_aggregates))
        .routes(utoipa_axum::routes!(
            handlers::get_provenance_lifecycle_events
        ))
        .routes(utoipa_axum::routes!(handlers::get_episode))
        .routes(utoipa_axum::routes!(handlers::get_episode_text))
        .routes(utoipa_axum::routes!(handlers::get_episode_stream))
        .routes(utoipa_axum::routes!(handlers::get_conversation_history))
        .routes(utoipa_axum::routes!(
            handlers::get_conversation_history_stream
        ))
        .split_for_parts();

    let public_router = agent_router.merge(other_public_router);
    let mut openapi = agent_openapi;
    openapi.merge(other_openapi);

    // ── Tier 2: Operator-authenticated routes (ClusterAuthLayer) ─
    // Config reads/mutations, secret management, deployment lifecycle, migration.
    let (operator_router, operator_openapi) = OpenApiRouter::new()
        .routes(utoipa_axum::routes!(config_handlers::list_secrets_overview))
        .routes(utoipa_axum::routes!(config_handlers::list_store_keys))
        .routes(utoipa_axum::routes!(config_handlers::list_config))
        .routes(utoipa_axum::routes!(config_handlers::get_config))
        .routes(utoipa_axum::routes!(config_handlers::list_config_versions))
        .routes(utoipa_axum::routes!(config_handlers::get_config_version))
        .routes(utoipa_axum::routes!(config_handlers::list_secret_requests))
        .routes(utoipa_axum::routes!(config_handlers::put_secret))
        .routes(utoipa_axum::routes!(config_handlers::delete_secret))
        .routes(utoipa_axum::routes!(config_handlers::put_config))
        .routes(utoipa_axum::routes!(config_handlers::delete_config))
        .routes(utoipa_axum::routes!(handlers::post_deploy))
        .routes(utoipa_axum::routes!(handlers::post_undeploy))
        .routes(utoipa_axum::routes!(handlers::get_deployments))
        .routes(utoipa_axum::routes!(handlers::post_migrate))
        .split_for_parts();

    let auth_layer = ClusterAuthLayer::new(ClusterAuthConfig {
        runner_token: runner_token.clone(),
        cluster_mode,
    });
    // Use `route_layer` so the auth check only runs for operator routes and
    // does NOT cover the fallback 404 handler. With `layer` the merged router
    // would 401 every unmatched path (including legitimate typos like
    // `/agents/<pkg>/default/a2a/sse`), masking routing bugs as auth failures.
    let operator_router = operator_router.route_layer(auth_layer.clone());

    openapi.merge(operator_openapi);
    let api_router = public_router.merge(operator_router);

    let mut openapi = openapi;

    // ── Repository API paths ────────────────────────────────────────
    // Only advertise /repository/* in the OpenAPI spec when the repository
    // service is actually wired, so the spec truthfully reflects the
    // mounted surface for each router entrypoint.
    let has_repository = repository_service.is_some();
    if has_repository {
        let repo_spec: utoipa::openapi::OpenApi =
            serde_json::from_value(repository_openapi_fragment())
                .expect("repository OpenAPI fragment is valid");
        openapi.merge(repo_spec);
    }

    // Register the RunnerToken security scheme so operator endpoints show auth
    // requirements in the OpenAPI spec.
    {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.security_schemes.insert(
            "RunnerToken".to_string(),
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-Runner-Token"))),
        );
    }

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
        Some(
            "Provenance-backed read APIs. UI observation should use conversation-history/episode snapshot+stream routes; /provenance/messages remains analytics-oriented."
                .to_string(),
        );
    let mut tag_deployments = utoipa::openapi::Tag::new("deployments");
    tag_deployments.description =
        Some("Runner-local deployment lifecycle APIs (deploy, undeploy, list).".to_string());
    let mut tag_config = utoipa::openapi::Tag::new("config");
    tag_config.description = Some(
        "Tool configuration and secret requests (schema includes config type schemas)".to_string(),
    );
    let mut tag_control = utoipa::openapi::Tag::new("control");
    tag_control.description =
        Some("Operational control plane: agent migration between runners.".to_string());
    let mut tags = vec![
        tag_agents,
        tag_mermaid,
        tag_provenance,
        tag_deployments,
        tag_config,
        tag_control,
    ];
    if has_repository {
        let mut tag_repository = utoipa::openapi::Tag::new("repository");
        tag_repository.description = Some(
            "Agent package repository: content-addressable archive with lineage, versioning, and search. Read routes are public; mutation routes (publish, fork, tags) require operator authentication."
                .to_string(),
        );
        tags.push(tag_repository);
    }
    openapi.tags = Some(tags);

    let state = Arc::new(ApiState {
        registry,
        openapi: Arc::new(openapi),
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        episode,
        conversation_history,
        conversation_history_events,
        context_index,
        deployment_manager,
        repository_url,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        ready,
        runner_token,
        cluster_mode,
    });

    let mut router = api_router
        .route("/openapi.json", axum::routing::get(serve_openapi_json))
        .route("/healthz", axum::routing::get(healthz))
        .route("/readyz", axum::routing::get(readyz))
        .route_layer(http_trace_layer)
        .with_state(state);

    if let Some(dir) = web_dir {
        let fallback = ServeDir::new(dir)
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(dir.join("index.html")));
        router = router.fallback_service(fallback);
    }
    if let Some(repo_service) = repository_service {
        // Read-only repository routes: public (agents, entries, lineage, blobs, search).
        let repo_read = baml_rt_repository::repository_read_router(repo_service.clone());

        // Mutation repository routes: operator-authenticated (fork, tags, publish).
        let publish_router = axum::Router::new()
            .route(
                "/publish",
                axum::routing::post(repository_publish::publish_with_build),
            )
            .with_state(repo_service.clone());
        let repo_mutations = baml_rt_repository::repository_mutation_router(repo_service)
            .merge(publish_router)
            .route_layer(auth_layer.clone());

        let repo_router = repo_read
            .merge(repo_mutations)
            .layer(axum::middleware::from_fn(repository_http_metrics));
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
        None,
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
    episode: Option<Arc<dyn EpisodeService>>,
    conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    conversation_history_events: Option<Arc<dyn crate::ConversationHistoryEventService>>,
    context_index: Option<Arc<dyn ContextIndexService>>,
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
        episode,
        conversation_history,
        conversation_history_events,
        context_index,
        None,
        None,
        None,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        Arc::new(AtomicBool::new(true)),
        None,
        ClusterMode::Standalone,
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
    episode: Option<Arc<dyn EpisodeService>>,
    conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    conversation_history_events: Option<Arc<dyn crate::ConversationHistoryEventService>>,
    context_index: Option<Arc<dyn ContextIndexService>>,
    deployment_manager: Option<Arc<dyn DeploymentManager>>,
    repository_url: Option<String>,
    repository_service: Option<Arc<baml_rt_repository::RepositoryService>>,
    tool_catalog: Arc<dyn ToolCatalog>,
    config_service: Arc<dyn ConfigService>,
    secret_resolver: Arc<dyn SecretResolver>,
    runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    ready: Arc<AtomicBool>,
    runner_token: Option<String>,
    cluster_mode: ClusterMode,
    web_dir: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = api_router_with_services_and_deploy(
        registry,
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        episode,
        conversation_history,
        conversation_history_events,
        context_index,
        deployment_manager,
        repository_url,
        repository_service,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        ready,
        runner_token,
        cluster_mode,
        web_dir,
    );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, web_dir = ?web_dir, "HTTP API listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the OpenAPI spec fragment for all `/repository/*` routes.
///
/// Read routes are public; mutation routes carry `RunnerToken` security.
/// Returned as a `serde_json::Value` so it can be deserialized into
/// `utoipa::openapi::OpenApi` and merged into the main spec without
/// requiring `ToSchema` derives on every repository domain type.
fn repository_openapi_fragment() -> serde_json::Value {
    let runner_token_security = json!([{"RunnerToken": []}]);
    json!({
        "openapi": "3.1.0",
        "info": { "title": "", "version": "" },
        "paths": {
            "/repository/agents": {
                "get": {
                    "tags": ["repository"],
                    "summary": "List agent names in the repository",
                    "operationId": "repository_list_agents",
                    "responses": {
                        "200": { "description": "List of agent names" }
                    }
                }
            },
            "/repository/agents/{name}/versions": {
                "get": {
                    "tags": ["repository"],
                    "summary": "List versions for an agent",
                    "operationId": "repository_list_versions",
                    "parameters": [{
                        "name": "name",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Agent name"
                    }],
                    "responses": {
                        "200": { "description": "Agent versions" },
                        "400": { "description": "Invalid agent name" }
                    }
                }
            },
            "/repository/entries": {
                "get": {
                    "tags": ["repository"],
                    "summary": "List repository entries with optional name/version filter",
                    "operationId": "repository_get_entries",
                    "parameters": [
                        { "name": "name", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Filter by agent name" },
                        { "name": "version", "in": "query", "required": false, "schema": { "type": "string" }, "description": "Filter by version (requires name)" }
                    ],
                    "responses": {
                        "200": { "description": "Matching entries" },
                        "400": { "description": "Invalid query" }
                    }
                }
            },
            "/repository/entries/{hash}": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Get a repository entry by content hash",
                    "operationId": "repository_get_by_hash",
                    "parameters": [{
                        "name": "hash",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Content hash"
                    }],
                    "responses": {
                        "200": { "description": "Repository entry" },
                        "400": { "description": "Invalid hash" },
                        "404": { "description": "Entry not found" }
                    }
                }
            },
            "/repository/entries/{name}/{version}": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Get a repository entry by agent name and version",
                    "operationId": "repository_get_by_version",
                    "parameters": [
                        { "name": "name", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Agent name" },
                        { "name": "version", "in": "path", "required": true, "schema": { "type": "string" }, "description": "Version number" }
                    ],
                    "responses": {
                        "200": { "description": "Repository entry" },
                        "400": { "description": "Invalid name or version" },
                        "404": { "description": "Entry not found" }
                    }
                }
            },
            "/repository/entries/{hash}/tags": {
                "post": {
                    "tags": ["repository"],
                    "summary": "Add a tag to a repository entry (operator-authenticated)",
                    "operationId": "repository_add_tag",
                    "parameters": [{
                        "name": "hash",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Content hash"
                    }],
                    "responses": {
                        "200": { "description": "Tag added" },
                        "400": { "description": "Invalid hash" },
                        "401": { "description": "Missing or invalid runner token" },
                        "404": { "description": "Entry not found" }
                    },
                    "security": runner_token_security
                },
                "delete": {
                    "tags": ["repository"],
                    "summary": "Remove a tag from a repository entry (operator-authenticated)",
                    "operationId": "repository_remove_tag",
                    "parameters": [{
                        "name": "hash",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Content hash"
                    }],
                    "responses": {
                        "200": { "description": "Tag removed" },
                        "400": { "description": "Invalid hash" },
                        "401": { "description": "Missing or invalid runner token" },
                        "404": { "description": "Entry not found" }
                    },
                    "security": runner_token_security
                }
            },
            "/repository/search": {
                "post": {
                    "tags": ["repository"],
                    "summary": "Search repository entries by metadata and content",
                    "operationId": "repository_search",
                    "responses": {
                        "200": { "description": "Search results" },
                        "500": { "description": "Search execution error" }
                    }
                }
            },
            "/repository/lineage/{hash}": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Get lineage subgraph for an entry",
                    "operationId": "repository_get_lineage",
                    "parameters": [{
                        "name": "hash",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Content hash"
                    }],
                    "responses": {
                        "200": { "description": "Lineage subgraph" },
                        "400": { "description": "Invalid hash" },
                        "404": { "description": "Lineage not found" }
                    }
                }
            },
            "/repository/blobs/{hash}": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Download the built artifact blob for an entry",
                    "operationId": "repository_get_blob",
                    "parameters": [{
                        "name": "hash",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Content hash"
                    }],
                    "responses": {
                        "200": {
                            "description": "Built artifact (application/gzip)",
                            "content": { "application/gzip": {} }
                        },
                        "400": { "description": "Invalid hash" },
                        "404": { "description": "Blob not found" }
                    }
                }
            },
            "/repository/publish": {
                "post": {
                    "tags": ["repository"],
                    "summary": "Publish an agent: build from source and store in repository (operator-authenticated)",
                    "operationId": "repository_publish",
                    "responses": {
                        "200": { "description": "Publish result with content hash" },
                        "400": { "description": "Invalid source bundle or hash mismatch" },
                        "401": { "description": "Missing or invalid runner token" },
                        "500": { "description": "Build or storage failure" }
                    },
                    "security": runner_token_security
                }
            },
            "/repository/fork": {
                "post": {
                    "tags": ["repository"],
                    "summary": "Fork an existing entry to create a new lineage branch (operator-authenticated)",
                    "operationId": "repository_fork",
                    "responses": {
                        "200": { "description": "Fork result" },
                        "401": { "description": "Missing or invalid runner token" },
                        "404": { "description": "Parent entry not found" },
                        "422": { "description": "Lineage violation" }
                    },
                    "security": runner_token_security
                }
            }
        }
    })
}
