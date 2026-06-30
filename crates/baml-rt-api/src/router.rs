// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Axum router and API state for the HTTP surface.
//!
//! Config, tool catalog, and secret resolver are **injected as required dependencies** (no Option).
//! Use [`api_router`] for a minimal router with in-memory config and empty catalog/resolver;
//! use [`api_router_with_services_and_deploy`] with an [`ApiServerConfig`] to inject real
//! implementations and (optionally) a deployment manager.

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
    cors::{Any, CorsLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa_axum::router::OpenApiRouter;

use crate::{
    ClusterHeartbeatHealth, ContextIndexService, ContextMetricsService, ConversationHistoryService,
    EpisodeService, HeartbeatStatus, MermaidService, PlanningService, ProvenanceOpsService,
    RuntimeProgressMeter, cluster_agents, cluster_agents::ClusterDirectoryService, cluster_deploy,
    config_handlers, external_tool_enable, handlers, metrics, otel_middleware, repository_publish,
};

/// Cluster-mode topology: either standalone (single runner, no peer awareness)
/// or a cluster of runners sharing a SurrealDB registry, in which case both
/// the directory (peer enumeration for `/cluster/agents`) and the heartbeat
/// monitor (surfaced via `/diagnose`) must be wired together.
///
/// Pairing them in a single enum makes the invariant "cluster mode iff both
/// dependencies present" impossible to violate in code. Match on this once
/// at the handler boundary instead of separately probing two `Option`s and
/// a flag, which would let a future caller wire one without the other.
#[derive(Clone)]
pub enum ClusterTopology {
    Standalone,
    Cluster {
        directory: Arc<dyn ClusterDirectoryService>,
        heartbeat: Arc<ClusterHeartbeatHealth>,
    },
}

impl ClusterTopology {
    /// Project to the `ClusterMode` the existing `ClusterAuthLayer` consumes.
    /// The auth layer only needs to know cluster-vs-standalone, not the
    /// directory/heartbeat handles — keep its API shape stable.
    pub fn mode(&self) -> ClusterMode {
        match self {
            Self::Standalone => ClusterMode::Standalone,
            Self::Cluster { .. } => ClusterMode::Cluster,
        }
    }

    /// Borrow the directory + heartbeat when in cluster mode.
    pub fn cluster_handles(
        &self,
    ) -> Option<(
        &Arc<dyn ClusterDirectoryService>,
        &Arc<ClusterHeartbeatHealth>,
    )> {
        match self {
            Self::Standalone => None,
            Self::Cluster {
                directory,
                heartbeat,
            } => Some((directory, heartbeat)),
        }
    }
}

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
    pub observation: Option<Arc<dyn crate::ObservationService>>,
    pub observation_events: Option<Arc<dyn crate::ObservationEventService>>,
    pub conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    pub context_index: Option<Arc<dyn ContextIndexService>>,
    pub deployment_manager: Option<Arc<dyn DeploymentManager>>,
    pub repository_url: Option<String>,
    pub tool_catalog: Arc<dyn ToolCatalog>,
    pub config_service: Arc<dyn ConfigService>,
    pub secret_resolver: Arc<dyn SecretResolver>,
    pub runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    /// Boot latch: `true` once event producers are registered (and poll loop spawned if configured).
    /// One input to the [`readyz`] gate; on its own does not guarantee the runtime is currently
    /// servable (see [`ApiState::runtime_progress`]). Use `/healthz` for liveness.
    pub ready: Arc<AtomicBool>,
    /// Shared secret for authenticating control-plane requests (e.g. `/control/migrate`).
    pub runner_token: Option<String>,
    /// Deployment topology. `Cluster` pairs the directory used by
    /// `GET /cluster/agents` with the heartbeat monitor surfaced by
    /// `GET /diagnose`; `Standalone` has neither. The enum prevents the
    /// "cluster mode but missing directory" invariant violation that
    /// flat option fields used to allow.
    pub cluster: ClusterTopology,
    /// Continuously observable signal of tokio-runtime progress; surfaced by `GET /diagnose`.
    /// Distinct from [`ApiState::ready`], which is a one-shot boot latch.
    pub runtime_progress: Arc<RuntimeProgressMeter>,
    /// Canonical host publish spine (poll lineage + fan-out + transport failures).
    pub host_publish: Option<Arc<baml_rt_core::HostPublishService>>,
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

/// Pod-readiness gate consumed by kubelet's readiness probe.
///
/// Returns `200 OK` iff every operator-visible servability signal is healthy:
/// * [`ApiState::ready`] — boot latch is set (event producers registered),
/// * [`ApiState::runtime_progress`] is within
///   [`READYZ_LAG_THRESHOLD_MS`](crate::READYZ_LAG_THRESHOLD_MS) — the
///   tokio runtime and (when wired) the QuickJS event loop are making
///   forward progress,
/// * the cluster heartbeat (when wired) has not gone from a previously
///   successful state to [`HeartbeatStatus::Degraded`] — see
///   [`ClusterHeartbeatHealth::is_within_readyz_threshold`]. Standalone
///   mode has no heartbeat; never-succeeded pods stay readyable so a
///   SurrealDB-slow-during-boot blip doesn't pin fresh pods at `503`.
///
/// Returns `503 Service Unavailable` otherwise. A runtime stall (cgroup
/// throttling, wedged QuickJS thread, deadlocked task) flips readiness to
/// false even while the listener is bound and accepting at the TCP level.
async fn readyz(axum::extract::State(state): axum::extract::State<Arc<ApiState>>) -> StatusCode {
    let start = Instant::now();
    let cluster_heartbeat_ok = state
        .cluster
        .cluster_handles()
        .is_none_or(|(_, hb)| hb.is_within_readyz_threshold());
    let code = if state.ready.load(Ordering::Acquire)
        && state.runtime_progress.is_within_readyz_threshold()
        && cluster_heartbeat_ok
    {
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

/// Cluster-heartbeat slice of `/diagnose`. Only present in cluster mode.
#[derive(serde::Serialize)]
struct ClusterHeartbeatDiagnose {
    status: HeartbeatStatus,
    /// `null` until the first successful heartbeat completes.
    lag_ms: Option<u64>,
    /// Class of the most recent failure (sticky-on-fault). Omitted before
    /// any error has been observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error_kind: Option<&'static str>,
}

#[derive(serde::Serialize)]
struct DiagnoseResponse {
    runtime_progress_lag_ms: u64,
    event_producers_loaded: bool,
    /// `None` in standalone mode; populated once per request from
    /// [`ApiState::cluster_heartbeat`] in cluster mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_heartbeat: Option<ClusterHeartbeatDiagnose>,
}

async fn diagnose(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::Json<DiagnoseResponse> {
    let start = Instant::now();
    let cluster_heartbeat =
        state
            .cluster
            .cluster_handles()
            .map(|(_, h)| ClusterHeartbeatDiagnose {
                status: h.status(),
                lag_ms: h.lag_millis(),
                last_error_kind: h.last_error_kind().map(|k| k.as_code()),
            });
    let body = DiagnoseResponse {
        runtime_progress_lag_ms: state.runtime_progress.lag_millis(),
        event_producers_loaded: state.ready.load(Ordering::Acquire),
        cluster_heartbeat,
    };
    metrics::record_request("get_diagnose", "success", start.elapsed());
    axum::Json(body)
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
        "/mcp/servers" => "repository_mcp_list_servers",
        "/mcp/servers/{server_id}" => "repository_mcp_get_latest_snapshot",
        "/mcp/servers/{server_id}/versions" => "repository_mcp_list_server_versions",
        "/mcp/servers/{server_id}/versions/{version}" => "repository_mcp_get_snapshot",
        "/mcp/servers/{server_id}/versions/{version}/mark-stale" => {
            "repository_mcp_mark_version_stale"
        }
        "/mcp/tools" => "repository_mcp_find_tool",
        "/mcp/snapshots/import" => "repository_mcp_import_snapshot",
        "/external-tools" => "repository_external_tool_list",
        "/external-tools/snapshots" => "repository_external_tool_list_snapshots",
        "/external-tools/snapshot" => "repository_external_tool_get_snapshot",
        "/external-tools/versions" => "repository_external_tool_list_versions",
        "/external-tools/snapshots/import" => "repository_external_tool_import_snapshot",
        "/external-tools/snapshots/mark-stale" => "repository_external_tool_mark_stale",
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

/// All API server inputs except the agent registry. Construct via
/// [`ApiServerConfig::empty`] and override the fields you care about with
/// struct-update syntax.
///
/// `runtime_progress` is shared with the runner so probes registered during
/// agent boot (e.g. the JS event-loop probe) feed the same `/diagnose` value
/// the HTTP API serves.
pub struct ApiServerConfig {
    pub mermaid: Option<Arc<dyn MermaidService>>,
    pub context_metrics: Option<Arc<dyn ContextMetricsService>>,
    pub provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    pub planning: Option<Arc<dyn PlanningService>>,
    pub episode: Option<Arc<dyn EpisodeService>>,
    pub observation: Option<Arc<dyn crate::ObservationService>>,
    pub observation_events: Option<Arc<dyn crate::ObservationEventService>>,
    pub conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    pub context_index: Option<Arc<dyn ContextIndexService>>,
    pub deployment_manager: Option<Arc<dyn DeploymentManager>>,
    pub repository_url: Option<String>,
    pub repository_service: Option<Arc<baml_rt_repository::RepositoryService>>,
    pub external_tool_sandbox:
        Option<baml_rt_tools::external_tools::resolver::SandboxRuntimeWiring>,
    pub tool_catalog: Arc<dyn ToolCatalog>,
    pub config_service: Arc<dyn ConfigService>,
    pub secret_resolver: Arc<dyn SecretResolver>,
    pub runtime_secret_store: Option<Arc<dyn RuntimeSecretStore>>,
    pub ready: Arc<AtomicBool>,
    pub runner_token: Option<String>,
    pub cluster: ClusterTopology,
    pub runtime_progress: Arc<RuntimeProgressMeter>,
    pub web_dir: Option<std::path::PathBuf>,
    /// Webhook intakes loaded from inventory via
    /// [`baml_rt_tools::load_configured_webhook_intakes`]. The router mounts
    /// each at its declared `mount_path`; operator-tier intakes inherit the
    /// runner-token auth layer applied to the operator route group.
    pub webhook_intakes: Vec<Arc<dyn baml_rt_tools::WebhookIntake>>,
    pub host_publish: Option<Arc<baml_rt_core::HostPublishService>>,
}

impl ApiServerConfig {
    /// Build a minimal config with only the four required infrastructure
    /// dependencies. All optional service injections default to `None`,
    /// `ready` to a latched `true`, and `cluster_mode` to standalone.
    pub fn empty(
        tool_catalog: Arc<dyn ToolCatalog>,
        config_service: Arc<dyn ConfigService>,
        secret_resolver: Arc<dyn SecretResolver>,
        runtime_progress: Arc<RuntimeProgressMeter>,
    ) -> Self {
        Self {
            mermaid: None,
            context_metrics: None,
            provenance_ops: None,
            planning: None,
            episode: None,
            observation: None,
            observation_events: None,
            conversation_history: None,
            context_index: None,
            deployment_manager: None,
            repository_url: None,
            repository_service: None,
            external_tool_sandbox: None,
            tool_catalog,
            config_service,
            secret_resolver,
            runtime_secret_store: None,
            ready: Arc::new(AtomicBool::new(true)),
            runner_token: None,
            cluster: ClusterTopology::Standalone,
            runtime_progress,
            web_dir: None,
            webhook_intakes: Vec::new(),
            host_publish: None,
        }
    }
}

/// Build a minimal API router with default config/catalog/resolver (in-memory
/// config, empty catalog, no-op resolver). For production, use
/// [`api_router_with_services_and_deploy`] with a populated [`ApiServerConfig`].
///
/// # Panics
/// Panics if called outside a tokio runtime context: the `/diagnose`
/// endpoint's progress meter spawns a background ticker on construction.
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
    let runtime_progress = RuntimeProgressMeter::spawn_in_current_runtime();
    let config = ApiServerConfig {
        mermaid,
        web_dir: web_dir.map(|p| p.to_path_buf()),
        ..ApiServerConfig::empty(
            tool_catalog,
            config_service,
            secret_resolver,
            runtime_progress,
        )
    };
    api_router_with_services_and_deploy(registry, config)
}

/// Build the full API router from an [`ApiServerConfig`].
pub fn api_router_with_services_and_deploy(
    registry: Arc<dyn AgentRegistry>,
    config: ApiServerConfig,
) -> Router {
    let ApiServerConfig {
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        observation,
        episode,
        conversation_history,
        observation_events,
        context_index,
        deployment_manager,
        repository_url,
        repository_service,
        external_tool_sandbox,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        ready,
        runner_token,
        cluster,
        runtime_progress,
        web_dir,
        webhook_intakes,
        host_publish,
    } = config;

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
        .routes(utoipa_axum::routes!(handlers::post_events_publish))
        .routes(utoipa_axum::routes!(handlers::list_agents))
        .routes(utoipa_axum::routes!(handlers::get_context_index))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_context))
        .routes(utoipa_axum::routes!(handlers::get_mermaid_task))
        .routes(utoipa_axum::routes!(handlers::get_context_metrics))
        .routes(utoipa_axum::routes!(handlers::get_context_planning))
        .routes(utoipa_axum::routes!(handlers::get_context_observe))
        .routes(utoipa_axum::routes!(handlers::get_context_observe_stream))
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
        .routes(utoipa_axum::routes!(
            crate::event_console::handlers::get_message_shapes
        ))
        .routes(utoipa_axum::routes!(
            crate::event_console::handlers::post_event_dispatch_validate
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
        .routes(utoipa_axum::routes!(cluster_agents::get_cluster_agents))
        .routes(utoipa_axum::routes!(cluster_deploy::post_cluster_deploy))
        .split_for_parts();

    let auth_layer = ClusterAuthLayer::new(ClusterAuthConfig {
        runner_token: runner_token.clone(),
        cluster_mode: cluster.mode(),
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
    let mut tag_cluster = utoipa::openapi::Tag::new("cluster");
    tag_cluster.description = Some(
        "Cluster-wide read views (e.g. /cluster/agents) that fan out across runner replicas."
            .to_string(),
    );
    let mut tags = vec![
        tag_agents,
        tag_mermaid,
        tag_provenance,
        tag_deployments,
        tag_config,
        tag_control,
        tag_cluster,
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

    // Override the OpenAPI `info` block. utoipa-axum otherwise fills it with its
    // own crate metadata — title "utoipa-axum" and the dependency author's
    // personal contact — which would ship verbatim in the published spec.
    {
        use utoipa::openapi::{Contact, Info, License};
        let mut info = Info::new("Agentium OS Runner API", env!("CARGO_PKG_VERSION"));
        info.description = Some(
            "HTTP API for the Agentium OS agent runner: agent discovery, A2A JSON-RPC \
             forwarding, deployment lifecycle, tool configuration, and provenance-backed \
             observability."
                .to_string(),
        );
        let mut contact = Contact::new();
        contact.name = Some("Agentium OS".to_string());
        contact.url = Some("https://github.com/semiotic-agentium/agentium-os".to_string());
        info.contact = Some(contact);
        info.license = Some(License::new("Apache-2.0"));
        openapi.info = info;
    }

    let state = Arc::new(ApiState {
        registry,
        openapi: Arc::new(openapi),
        mermaid,
        context_metrics,
        provenance_ops,
        planning,
        observation,
        episode,
        conversation_history,
        observation_events,
        context_index,
        deployment_manager,
        repository_url,
        tool_catalog,
        config_service,
        secret_resolver,
        runtime_secret_store,
        ready,
        runner_token,
        cluster,
        runtime_progress,
        host_publish,
    });

    let mut router = api_router
        .route("/openapi.json", axum::routing::get(serve_openapi_json))
        .route("/healthz", axum::routing::get(healthz))
        .route("/readyz", axum::routing::get(readyz))
        .route("/diagnose", axum::routing::get(diagnose))
        .route_layer(http_trace_layer)
        .with_state(state);

    // ── Webhook intakes from inventory (push sources: Grafana, ...).
    // Mounted after with_state so the intakes (which carry their own per-route
    // state via with_state(intake)) merge into a state-discharged Router<()>.
    // The operator arm gets the same X-Runner-Token auth as /config, /deploy
    // via route_layer; the public arm is reachable without auth like /chat.
    if !webhook_intakes.is_empty() {
        let webhook_intake_count = webhook_intakes.len();
        let crate::webhook_mount::WebhookIntakeRouters {
            public: webhook_public,
            operator: webhook_operator,
        } = crate::webhook_mount::build_webhook_intake_router(webhook_intakes);
        if let Some(public) = webhook_public {
            router = router.merge(public);
        }
        if let Some(operator) = webhook_operator {
            router = router.merge(operator.route_layer(auth_layer.clone()));
        }
        tracing::info!(
            webhook_intake_count,
            "mounted webhook intakes from inventory"
        );
    }

    if let Some(dir) = web_dir.as_deref() {
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
        let enable_external_tool_router = axum::Router::new()
            .route(
                "/external-tools/enable",
                axum::routing::post(external_tool_enable::enable_external_tool),
            )
            .with_state(external_tool_enable::ExternalToolEnableState {
                repository: repo_service.clone(),
                sandbox: external_tool_sandbox,
            });
        let repo_mutations = baml_rt_repository::repository_mutation_router(repo_service)
            .merge(publish_router)
            .merge(enable_external_tool_router)
            .route_layer(auth_layer.clone());

        let repo_router = repo_read
            .merge(repo_mutations)
            .layer(axum::middleware::from_fn(repository_http_metrics));
        router = router.nest("/repository", repo_router);
    }

    if let Some(cors) = optional_cors_layer() {
        router = router.layer(cors);
    }

    router
}

/// Opt-in CORS for standalone dev consoles (`AGENTIUM_CORS_ORIGINS=http://localhost:5173,...`).
fn optional_cors_layer() -> Option<CorsLayer> {
    let raw = std::env::var("AGENTIUM_CORS_ORIGINS").ok()?;
    let origins: Vec<_> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any),
    )
}

/// Run the HTTP server with the full [`ApiServerConfig`].
pub async fn serve_with_services_and_deploy(
    registry: Arc<dyn AgentRegistry>,
    bind: &str,
    config: ApiServerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(bind = %bind, web_dir = ?config.web_dir, "HTTP API binding");
    let app = api_router_with_services_and_deploy(registry, config);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "HTTP API listening");

    // Issue #341 T4 fault injection: a debug-only hook that lets integration
    // tests force `axum::serve` to return `Ok(())` after a delay, simulating a
    // listener task that exits silently. Stripped from release binaries via
    // `cfg(debug_assertions)` so production K8s images cannot be coerced into
    // the early-exit path. Activated by setting the env var named by
    // `LISTENER_EXIT_AFTER_SECS_ENV` to a `u64` number of seconds.
    #[cfg(debug_assertions)]
    match std::env::var(LISTENER_EXIT_AFTER_SECS_ENV) {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(secs) => {
                tracing::warn!(
                    env = LISTENER_EXIT_AFTER_SECS_ENV,
                    secs,
                    "TEST: listener fault-injection env set; listener will return Ok(()) after timeout (issue #341 T4)"
                );
                axum::serve(listener, app)
                    .with_graceful_shutdown(tokio::time::sleep(std::time::Duration::from_secs(
                        secs,
                    )))
                    .await?;
                return Ok(());
            }
            Err(err) => tracing::warn!(
                env = LISTENER_EXIT_AFTER_SECS_ENV,
                value = %raw,
                error = %err,
                "TEST: listener fault-injection env is set but does not parse as u64; ignoring"
            ),
        },
        Err(std::env::VarError::NotPresent) => {}
        Err(std::env::VarError::NotUnicode(raw)) => tracing::warn!(
            env = LISTENER_EXIT_AFTER_SECS_ENV,
            value = ?raw,
            "TEST: listener fault-injection env is set but is not valid Unicode; ignoring"
        ),
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// Env var that activates the issue #341 T4 listener-fault-injection hook.
/// Setting this to a `u64` (number of seconds) on the runner process makes the
/// HTTP listener gracefully shut down after that delay and return `Ok(())`,
/// simulating a silent listener-task death. The hook itself is gated behind
/// `cfg(debug_assertions)` and is stripped from release binaries.
pub const LISTENER_EXIT_AFTER_SECS_ENV: &str = "AGENTIUM_TEST_LISTENER_EXIT_AFTER_SECS";

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
            "/repository/mcp/servers": {
                "get": {
                    "tags": ["repository"],
                    "summary": "List MCP servers",
                    "operationId": "repository_mcp_list_servers",
                    "responses": { "200": { "description": "MCP server list" } }
                }
            },
            "/repository/mcp/servers/{server_id}": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Get latest MCP server snapshot",
                    "operationId": "repository_mcp_get_latest_snapshot",
                    "parameters": [{ "name": "server_id", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": {
                        "200": { "description": "MCP server snapshot" },
                        "404": { "description": "MCP server snapshot not found" }
                    }
                }
            },
            "/repository/mcp/servers/{server_id}/versions": {
                "get": {
                    "tags": ["repository"],
                    "summary": "List MCP server snapshot versions",
                    "operationId": "repository_mcp_list_server_versions",
                    "parameters": [{ "name": "server_id", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "MCP server versions" } }
                }
            },
            "/repository/mcp/servers/{server_id}/versions/{version}": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Get MCP server snapshot by version",
                    "operationId": "repository_mcp_get_snapshot",
                    "parameters": [
                        { "name": "server_id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "version", "in": "path", "required": true, "schema": { "type": "integer", "format": "uint32" } }
                    ],
                    "responses": {
                        "200": { "description": "MCP server snapshot" },
                        "404": { "description": "MCP server snapshot not found" }
                    }
                }
            },
            "/repository/mcp/servers/{server_id}/versions/{version}/mark-stale": {
                "post": {
                    "tags": ["repository"],
                    "summary": "Mark MCP server snapshot version stale (operator-authenticated)",
                    "operationId": "repository_mcp_mark_version_stale",
                    "parameters": [
                        { "name": "server_id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "version", "in": "path", "required": true, "schema": { "type": "integer", "format": "uint32" } }
                    ],
                    "responses": {
                        "200": { "description": "MCP server snapshot marked stale" },
                        "401": { "description": "Missing or invalid runner token" }
                    },
                    "security": runner_token_security
                }
            },
            "/repository/mcp/tools": {
                "get": {
                    "tags": ["repository"],
                    "summary": "Find MCP tool versions by platform tool name",
                    "operationId": "repository_mcp_find_tool",
                    "parameters": [{ "name": "platform_tool_name", "in": "query", "required": true, "schema": { "type": "string" } }],
                    "responses": {
                        "200": { "description": "MCP tool versions" },
                        "400": { "description": "Invalid query" }
                    }
                }
            },
            "/repository/mcp/snapshots/import": {
                "post": {
                    "tags": ["repository"],
                    "summary": "Import MCP server snapshot (operator-authenticated)",
                    "operationId": "repository_mcp_import_snapshot",
                    "responses": {
                        "200": { "description": "Imported MCP snapshot version" },
                        "400": { "description": "Invalid snapshot" },
                        "401": { "description": "Missing or invalid runner token" }
                    },
                    "security": runner_token_security
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
