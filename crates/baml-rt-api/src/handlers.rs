//! HTTP handlers: discovery, A2A forward (POST), and deterministic dispatch.

use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode as AxumStatus,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use baml_rt_core::{
    A2aWireRequest, AgentDispatchRequest, AgentInstanceId, AgentPackageName, AgentRouteKey,
    BamlRtError, DeploymentContentHash, DeploymentStatus, collect_a2a_stream,
    ids::{AgentId, ContextId, TaskId},
};
use baml_rt_provenance::{
    ProvenanceOpsFilters, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    ProvenanceOutcomeSegment, ProvenanceResponseProfile,
};
use futures_util::stream::Stream;
use http_api_problem::HttpApiProblem;
use serde_json::Value;

use crate::{
    ApiState,
    context_index::{
        ContextIndexQueryParams, ContextIndexRequest, ContextIndexRequestParseError,
        ContextPickerPageDto,
    },
    context_metrics::{ContextMetricsError, ContextMetricsResponseDto},
    conversation_history::{
        ConversationHistoryDeltaRequest, ConversationHistoryPageDto,
        ConversationHistoryQueryParams, ConversationHistoryRequest,
        ConversationHistoryRequestParseError,
    },
    episode::EpisodeSnapshotDto,
    mermaid::MermaidError,
    metrics,
    openapi::{
        AgentDiscoveryEntryDto, DeployRequestDto, DeployResponseDto, DeploymentRecordDto,
        MigrateRequestDto, MigrateResponseDto, UndeployRequestDto, UndeployResponseDto,
    },
    planning::{ContextPlanningResponse, PlanningError},
    provenance_ops::ProvenanceOpsError,
    service_error::service_result_to_http,
    spans,
};

/// HTTP result type for handlers that return RFC 7807 problem details on error.
type HttpResult<T> = Result<T, HttpApiProblem>;

#[derive(Debug, serde::Deserialize)]
struct RepositoryEntriesResponse {
    entries: Vec<RepositoryEntryHeaderItem>,
}

#[derive(Debug, serde::Deserialize)]
struct RepositoryEntryHeaderItem {
    hash: String,
}

/// Build HttpApiProblem for standard HTTP status codes (4xx/5xx per RFC 7231).
/// All standard status codes are valid; the expect here cannot fail.
fn problem(status: u16, title: &str, detail: impl Into<String>) -> HttpApiProblem {
    HttpApiProblem::try_new(status)
        .expect("standard HTTP status codes are always valid")
        .title(title)
        .detail(detail)
}

fn bad_request_problem(detail: impl Into<String>) -> HttpApiProblem {
    problem(400, "Bad Request", detail)
}

fn deployment_status_to_str(status: DeploymentStatus) -> &'static str {
    match status {
        DeploymentStatus::Active => "active",
        DeploymentStatus::Failed => "failed",
    }
}

async fn resolve_deploy_hash(state: &Arc<ApiState>, body: DeployRequestDto) -> HttpResult<String> {
    if let Some(hash) = body.hash {
        if hash.trim().is_empty() {
            return Err(problem(400, "Bad Request", "hash must not be empty"));
        }
        return Ok(hash);
    }

    let name = body.name.filter(|v| !v.trim().is_empty()).ok_or_else(|| {
        problem(
            400,
            "Bad Request",
            "Either hash or name+version must be provided",
        )
    })?;
    let version = body
        .version
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            problem(
                400,
                "Bad Request",
                "version is required when name is provided",
            )
        })?;
    let Some(repository_url) = state.repository_url.as_ref() else {
        return Err(problem(
            501,
            "Not Implemented",
            "repository_url is not configured for name/version deploy",
        ));
    };

    let url = format!("{}/entries", repository_url.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| {
            problem(
                500,
                "Internal Server Error",
                format!("HTTP client build: {e}"),
            )
        })?
        .get(&url)
        .query(&[("name", name.as_str()), ("version", version.as_str())])
        .send()
        .await
        .map_err(|e| {
            problem(
                500,
                "Internal Server Error",
                format!("Failed resolving name/version: {e}"),
            )
        })?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(problem(
            404,
            "Not Found",
            format!("No repository entry found for {name}@{version}"),
        ));
    }
    if !response.status().is_success() {
        let status = response.status();
        let body_text =
            baml_rt_router::ssrf::truncate_body(&response.text().await.unwrap_or_default(), 512);
        return Err(problem(
            500,
            "Internal Server Error",
            format!("Repository lookup failed ({status}): {body_text}"),
        ));
    }

    let body_json = response
        .json::<RepositoryEntriesResponse>()
        .await
        .map_err(|e| {
            problem(
                500,
                "Internal Server Error",
                format!("Invalid repository response: {e}"),
            )
        })?;
    let hash = body_json
        .entries
        .first()
        .map(|entry| entry.hash.clone())
        .ok_or_else(|| {
            problem(
                404,
                "Not Found",
                format!("No repository entry found for {name}@{version}"),
            )
        })?;
    Ok(hash)
}

fn result_label_for_domain_error(error: &BamlRtError) -> &'static str {
    match error {
        BamlRtError::Conflict(_) => "conflict",
        BamlRtError::InvalidArgument(msg) if msg.contains("not found") => "not_found",
        BamlRtError::InvalidArgument(_) => "bad_request",
        BamlRtError::SessionLifecycle(
            baml_rt_core::SessionLifecycleError::ToolSessionNotFound { .. },
        )
        | BamlRtError::SessionLifecycle(
            baml_rt_core::SessionLifecycleError::StreamSessionNotFound { .. },
        ) => "not_found",
        BamlRtError::SessionLifecycle(_) => "bad_request",
        BamlRtError::FunctionNotFound(_) => "not_found",
        _ => "internal",
    }
}

/// List running agents (GET /agents).
#[utoipa::path(
    get,
    path = "/agents",
    tag = "agents",
    responses((status = 200, description = "List of running agents", body = [AgentDiscoveryEntryDto]))
)]
pub async fn list_agents(
    State(state): State<Arc<ApiState>>,
) -> (AxumStatus, Json<Vec<AgentDiscoveryEntryDto>>) {
    let span = spans::list_agents();
    let _guard = span.enter();
    let start = Instant::now();
    let entries = state.registry.list_agents();
    let dtos = entries
        .into_iter()
        .map(AgentDiscoveryEntryDto::from)
        .collect();
    metrics::record_request("list_agents", "success", start.elapsed());
    (AxumStatus::OK, Json(dtos))
}

/// Deploy an agent by content hash or name/version.
#[utoipa::path(
    post,
    path = "/deploy",
    tag = "deployments",
    request_body = DeployRequestDto,
    responses(
        (status = 200, description = "Deployment accepted", body = DeployResponseDto),
        (status = 400, description = "Invalid deploy payload"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Hash or version target not found"),
        (status = 409, description = "Deployment conflict"),
        (status = 501, description = "Deployment manager not configured"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn post_deploy(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<DeployRequestDto>,
) -> HttpResult<Json<DeployResponseDto>> {
    let Some(manager) = &state.deployment_manager else {
        return Err(problem(
            501,
            "Not Implemented",
            "Deployment manager not configured",
        ));
    };

    let hash = resolve_deploy_hash(&state, body).await?;
    let content_hash = hash
        .parse::<DeploymentContentHash>()
        .map_err(|e| problem(400, "Bad Request", format!("invalid hash: {e}")))?;
    let deploy_result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(manager.deploy_by_hash(&content_hash))
    });
    match deploy_result {
        Ok(result) => Ok(Json(DeployResponseDto {
            hash,
            already_deployed: result.already_deployed,
        })),
        Err(BamlRtError::AgentNotFound(msg)) => Err(problem(404, "Not Found", msg)),
        Err(BamlRtError::InvalidArgument(msg)) => Err(problem(400, "Bad Request", msg)),
        Err(BamlRtError::Conflict(msg)) => Err(problem(409, "Conflict", msg)),
        Err(e) => Err(problem(500, "Internal Server Error", e.to_string())),
    }
}

/// Undeploy an active deployment by content hash.
#[utoipa::path(
    post,
    path = "/undeploy",
    tag = "deployments",
    request_body = UndeployRequestDto,
    responses(
        (status = 200, description = "Undeploy accepted", body = UndeployResponseDto),
        (status = 400, description = "Invalid undeploy payload"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Deployment not found"),
        (status = 501, description = "Deployment manager not configured"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn post_undeploy(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UndeployRequestDto>,
) -> HttpResult<Json<UndeployResponseDto>> {
    let Some(manager) = &state.deployment_manager else {
        return Err(problem(
            501,
            "Not Implemented",
            "Deployment manager not configured",
        ));
    };

    let content_hash = body
        .hash
        .parse::<DeploymentContentHash>()
        .map_err(|e| problem(400, "Bad Request", format!("invalid hash: {e}")))?;
    let undeploy_result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(manager.undeploy_by_hash(&content_hash))
    });
    match undeploy_result {
        Ok(result) if result.removed => Ok(Json(UndeployResponseDto { removed: true })),
        Ok(_) => Err(problem(
            404,
            "Not Found",
            format!("deployment not found for hash {hash}", hash = body.hash),
        )),
        Err(BamlRtError::InvalidArgument(msg)) => Err(problem(400, "Bad Request", msg)),
        Err(e) => Err(problem(500, "Internal Server Error", e.to_string())),
    }
}

/// List runner-local deployment records.
#[utoipa::path(
    get,
    path = "/deployments",
    tag = "deployments",
    responses(
        (status = 200, description = "Deployment records", body = [DeploymentRecordDto]),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 501, description = "Deployment manager not configured"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_deployments(
    State(state): State<Arc<ApiState>>,
) -> HttpResult<Json<Vec<DeploymentRecordDto>>> {
    let Some(manager) = &state.deployment_manager else {
        return Err(problem(
            501,
            "Not Implemented",
            "Deployment manager not configured",
        ));
    };
    let deployments_result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(manager.list_deployments())
    });
    match deployments_result {
        Ok(records) => Ok(Json(
            records
                .into_iter()
                .map(|record| DeploymentRecordDto {
                    content_hash: record.content_hash.as_str().to_string(),
                    agent_name: record.agent_name,
                    deployed_at: record.deployed_at,
                    status: deployment_status_to_str(record.status).to_string(),
                    last_error: record.last_error,
                    last_attempt_at: record.last_attempt_at,
                    failure_count: record.failure_count,
                })
                .collect(),
        )),
        Err(e) => Err(problem(500, "Internal Server Error", e.to_string())),
    }
}

/// Migrate an agent from this runner to a target runner.
///
/// Drains the agent locally (waits for in-flight turns), then tells the target
/// runner to deploy the same content hash.
#[utoipa::path(
    post,
    path = "/control/migrate",
    request_body = MigrateRequestDto,
    responses(
        (status = 200, description = "Agent migrated successfully", body = MigrateResponseDto),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Missing or invalid runner token"),
        (status = 404, description = "Agent not found locally"),
        (status = 501, description = "Deployment manager not configured"),
        (status = 502, description = "Target runner unreachable"),
    ),
    tag = "control"
)]
pub async fn post_migrate(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<MigrateRequestDto>,
) -> HttpResult<Json<MigrateResponseDto>> {
    let Some(manager) = &state.deployment_manager else {
        return Err(problem(
            501,
            "Not Implemented",
            "Deployment manager not configured",
        ));
    };

    let hash = &body.hash;
    let target = &body.target_runner_endpoint;

    // SSRF protection: validate target endpoint and resolve DNS to block
    // private/metadata IPs behind attacker-controlled hostnames. Pin resolved
    // IPs to close the DNS-rebinding TOCTOU gap.
    let (target_url, resolved_addrs) =
        baml_rt_router::ssrf::resolve_and_validate_cluster_endpoint(target)
            .await
            .map_err(|e| problem(400, "Bad Request", e))?;

    let content_hash = hash
        .parse::<DeploymentContentHash>()
        .map_err(|e| problem(400, "Bad Request", format!("invalid hash: {e}")))?;

    // 1. Forward deploy to target runner FIRST (before local undeploy).
    // Use `host()` (not `host_str()`) to get the unbracketed form for IPv6
    // literals — hyper's DNS override map is keyed on the bare address.
    let host = match target_url.host() {
        Some(url::Host::Domain(d)) => d.to_string(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => String::new(),
    };
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none());
    builder = builder.resolve_to_addrs(&host, &resolved_addrs);
    let client = builder.build().map_err(|e| {
        problem(
            500,
            "Internal Server Error",
            format!("HTTP client build: {e}"),
        )
    })?;

    let base = baml_rt_router::ssrf::origin_url(&target_url);
    let deploy_url = format!("{base}/deploy");
    let deploy_body = serde_json::json!({ "hash": hash });
    let mut req = client.post(&deploy_url).json(&deploy_body);
    if let Some(token) = &state.runner_token {
        req = req.header("X-Runner-Token", token.as_str());
    }
    let resp = req.send().await.map_err(|e| {
        problem(
            502,
            "Bad Gateway",
            format!("failed to reach target runner: {e}"),
        )
    })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable>".to_string());
        let text = baml_rt_router::ssrf::truncate_body(&text, 512);
        return Err(problem(
            502,
            "Bad Gateway",
            format!("target runner returned {status}: {text}"),
        ));
    }

    // 2. Target confirmed deploy success — now drain and undeploy locally.
    let undeploy_result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(manager.undeploy_by_hash(&content_hash))
    });
    let source_undeploy_failed = match &undeploy_result {
        Ok(r) if !r.removed => {
            tracing::warn!(
                %hash,
                "agent deployed to target but not found locally for undeploy"
            );
            false
        }
        Err(e) => {
            tracing::error!(
                %hash,
                error = %e,
                "agent deployed to target but local undeploy failed"
            );
            true
        }
        _ => false,
    };

    tracing::info!(%hash, target = %target_url, source_undeploy_failed, "agent migrated");
    if source_undeploy_failed {
        // Agent is running on both source and target — callers must not
        // treat this as a clean migration.
        return Err(problem(
            500,
            "Internal Server Error",
            "agent deployed to target but local undeploy failed; agent may be running on both runners",
        ));
    }
    Ok(Json(MigrateResponseDto {
        migrated: true,
        source_undeploy_failed,
    }))
}

/// Forward A2A JSON-RPC request (POST /agents/{agent_package}/{agent_instance_id}/a2a).
///
/// Unauthenticated at the application layer. Cluster isolation relies on
/// network policy (K8s NetworkPolicy / service mesh); external client auth
/// belongs at the ingress. Control-plane endpoints are protected by `ClusterAuthLayer`.
#[utoipa::path(
    post,
    path = "/agents/{agent_package}/{agent_instance_id}/a2a",
    tag = "agents",
    params(
        ("agent_package" = String, Path, description = "Agent package identifier (e.g. manifest name)"),
        ("agent_instance_id" = String, Path, description = "Agent instance identifier")
    ),
    request_body = Value,
    responses(
        (status = 200, description = "JSON-RPC responses", body = [Value]),
        (status = 400, description = "Malformed request"),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn post_a2a(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((agent_package, agent_instance_id)): axum::extract::Path<(String, String)>,
    Json(body): Json<Value>,
) -> HttpResult<Json<Vec<Value>>> {
    let span = spans::post_a2a(&agent_package, &agent_instance_id);
    let _guard = span.enter();
    let start = Instant::now();
    let package_name = AgentPackageName::parse(&agent_package)
        .ok_or_else(|| problem(400, "Bad Request", "agent_package must match [A-Za-z0-9_-]"))?;
    let instance_id = AgentInstanceId::parse(&agent_instance_id).ok_or_else(|| {
        problem(
            400,
            "Bad Request",
            "agent_instance_id must match [A-Za-z0-9_-]",
        )
    })?;
    let key = AgentRouteKey::new(package_name, instance_id);

    if !body.is_object() {
        metrics::record_request("post_a2a", "bad_request", start.elapsed());
        return Err(problem(
            400,
            "Bad Request",
            "Body must be a JSON object (JSON-RPC request)",
        ));
    }

    match state
        .registry
        .handle_a2a_stream(&key, A2aWireRequest::from(body))
        .await
    {
        Ok(stream) => {
            let responses = collect_a2a_stream(stream)
                .await
                .into_iter()
                .map(|chunk| chunk.into_inner())
                .collect();
            metrics::record_request("post_a2a", "success", start.elapsed());
            Ok(Json(responses))
        }
        Err(e) => {
            metrics::record_request(
                "post_a2a",
                result_label_for_domain_error(&e),
                start.elapsed(),
            );
            Err(domain_to_problem(&e, &agent_package, &agent_instance_id))
        }
    }
}

/// Get provenance graph as a Mermaid sequence diagram for an A2A context.
#[utoipa::path(
    get,
    path = "/contexts/{context_id}/mermaid",
    tag = "mermaid",
    summary = "Mermaid diagram by context",
    description = "Returns the provenance subgraph for the given A2A context ID as a Mermaid sequenceDiagram (text/plain). Available when the runner is started with SurrealDB provenance.",
    params(("context_id" = String, Path, description = "A2A context ID")),
    responses(
        (status = 200, description = "Mermaid sequenceDiagram", content_type = "text/plain"),
        (status = 404, description = "No graph found for context"),
        (status = 501, description = "Mermaid service not available (provenance not configured)"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_mermaid_context(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(context_id): axum::extract::Path<String>,
) -> HttpResult<axum::response::Response> {
    let span = spans::get_mermaid_context(&context_id);
    let _guard = span.enter();
    let start = Instant::now();
    let Some(svc) = &state.mermaid else {
        metrics::record_request("get_mermaid_context", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Mermaid service not configured",
        ));
    };
    match svc.mermaid_for_context(&context_id).await {
        Ok(diagram) => {
            metrics::record_request("get_mermaid_context", "success", start.elapsed());
            Ok((
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                diagram,
            )
                .into_response())
        }
        Err(MermaidError::NotFound) => {
            metrics::record_request("get_mermaid_context", "not_found", start.elapsed());
            Err(problem(
                404,
                "Not Found",
                format!("no graph for context {context_id}"),
            ))
        }
        Err(MermaidError::Unavailable) => {
            metrics::record_request("get_mermaid_context", "unavailable", start.elapsed());
            Err(problem(
                501,
                "Not Implemented",
                "Mermaid service unavailable",
            ))
        }
        Err(MermaidError::Other(e)) => {
            metrics::record_request("get_mermaid_context", "internal", start.elapsed());
            Err(problem(500, "Internal Server Error", e.to_string()))
        }
    }
}

/// Get provenance graph as a Mermaid sequence diagram for an A2A task.
#[utoipa::path(
    get,
    path = "/tasks/{task_id}/mermaid",
    tag = "mermaid",
    summary = "Mermaid diagram by task",
    description = "Returns the provenance subgraph for the given A2A task ID as a Mermaid sequenceDiagram (text/plain). Available when the runner is started with SurrealDB provenance.",
    params(("task_id" = String, Path, description = "A2A task ID")),
    responses(
        (status = 200, description = "Mermaid sequenceDiagram", content_type = "text/plain"),
        (status = 404, description = "No graph found for task"),
        (status = 501, description = "Mermaid service not available (provenance not configured)"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_mermaid_task(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> HttpResult<axum::response::Response> {
    let span = spans::get_mermaid_task(&task_id);
    let _guard = span.enter();
    let start = Instant::now();
    let Some(svc) = &state.mermaid else {
        metrics::record_request("get_mermaid_task", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Mermaid service not configured",
        ));
    };
    match svc.mermaid_for_task(&task_id).await {
        Ok(diagram) => {
            metrics::record_request("get_mermaid_task", "success", start.elapsed());
            Ok((
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                diagram,
            )
                .into_response())
        }
        Err(MermaidError::NotFound) => {
            metrics::record_request("get_mermaid_task", "not_found", start.elapsed());
            Err(problem(
                404,
                "Not Found",
                format!("no graph for task {task_id}"),
            ))
        }
        Err(MermaidError::Unavailable) => {
            metrics::record_request("get_mermaid_task", "unavailable", start.elapsed());
            Err(problem(
                501,
                "Not Implemented",
                "Mermaid service unavailable",
            ))
        }
        Err(MermaidError::Other(e)) => {
            metrics::record_request("get_mermaid_task", "internal", start.elapsed());
            Err(problem(500, "Internal Server Error", e.to_string()))
        }
    }
}

/// Get context token/call metrics aggregated from provenance graph data.
#[utoipa::path(
    get,
    path = "/contexts/{context_id}/metrics",
    tag = "provenance",
    summary = "Context metrics by context_id",
    description = "Returns turn-level and session-level token/call/duration metrics for the given A2A context ID. Available when SurrealDB-backed provenance is configured.",
    params(("context_id" = String, Path, description = "A2A context ID")),
    responses(
        (status = 200, description = "Context metrics", body = ContextMetricsResponseDto),
        (status = 404, description = "No metrics found for context"),
        (status = 501, description = "Metrics service not available (provenance not configured)"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_context_metrics(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(context_id): axum::extract::Path<String>,
) -> HttpResult<Json<ContextMetricsResponseDto>> {
    let span = spans::get_context_metrics(&context_id);
    let _guard = span.enter();
    let start = Instant::now();
    let Some(svc) = &state.context_metrics else {
        metrics::record_request("get_context_metrics", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Context metrics service not configured",
        ));
    };
    match svc.metrics_for_context(&context_id).await {
        Ok(report) => {
            metrics::record_request("get_context_metrics", "success", start.elapsed());
            Ok(Json(report))
        }
        Err(ContextMetricsError::NotFound) => {
            metrics::record_request("get_context_metrics", "not_found", start.elapsed());
            Err(problem(
                404,
                "Not Found",
                format!("no metrics for context {context_id}"),
            ))
        }
        Err(ContextMetricsError::Unavailable) => {
            metrics::record_request("get_context_metrics", "unavailable", start.elapsed());
            Err(problem(
                501,
                "Not Implemented",
                "Context metrics service unavailable",
            ))
        }
        Err(ContextMetricsError::Other(e)) => {
            metrics::record_request("get_context_metrics", "internal", start.elapsed());
            Err(problem(500, "Internal Server Error", e.to_string()))
        }
    }
}

/// Get context-scoped planning state (intent/plan/step progress) from provenance.
#[utoipa::path(
    get,
    path = "/contexts/{context_id}/planning",
    tag = "provenance",
    params(
        ("context_id" = String, Path, description = "A2A context id")
    ),
    responses(
        (status = 200, description = "Context planning snapshot", body = Value),
        (status = 404, description = "No planning data found for context"),
        (status = 501, description = "Planning service unavailable"),
        (status = 500, description = "Internal error"),
    )
)]
pub async fn get_context_planning(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(context_id): axum::extract::Path<String>,
) -> HttpResult<Json<ContextPlanningResponse>> {
    let span = spans::get_context_planning(&context_id);
    let _guard = span.enter();
    let start = Instant::now();
    let Some(svc) = &state.planning else {
        metrics::record_request("get_context_planning", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Planning service not configured",
        ));
    };
    match svc.planning_for_context(&context_id).await {
        Ok(resp) => {
            metrics::record_request("get_context_planning", "success", start.elapsed());
            Ok(Json(resp))
        }
        Err(PlanningError::NotFound) => {
            metrics::record_request("get_context_planning", "not_found", start.elapsed());
            Err(problem(
                404,
                "Not Found",
                format!("no planning data for context {context_id}"),
            ))
        }
        Err(PlanningError::Unavailable) => {
            metrics::record_request("get_context_planning", "unavailable", start.elapsed());
            Err(problem(
                501,
                "Not Implemented",
                "Planning service unavailable",
            ))
        }
        Err(PlanningError::Other(e)) => {
            metrics::record_request("get_context_planning", "internal", start.elapsed());
            Err(problem(500, "Internal Server Error", e.to_string()))
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQueryParams {
    pub context_id: Option<ContextId>,
    pub task_id: Option<TaskId>,
    pub agent_id: Option<AgentId>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tool_name: Option<String>,
    pub baml_prompt: Option<String>,
    pub payload_text: Option<String>,
    pub from_timestamp_ms: Option<u64>,
    pub to_timestamp_ms: Option<u64>,
    pub group_by: Option<String>,
    pub sort_by: Option<String>,
    pub sort_dir: Option<String>,
    pub page_size: Option<u32>,
    pub cursor: Option<String>,
    pub top_k: Option<u32>,
    pub outcome: Option<String>,
    pub response_profile: Option<String>,
}

fn parse_outcome(raw: Option<&str>) -> ProvenanceOutcomeSegment {
    match raw.unwrap_or("both").to_ascii_lowercase().as_str() {
        "failed_only" => ProvenanceOutcomeSegment::FailedOnly,
        "successful_only" => ProvenanceOutcomeSegment::SuccessfulOnly,
        _ => ProvenanceOutcomeSegment::Both,
    }
}

fn parse_profile(raw: Option<&str>) -> ProvenanceResponseProfile {
    match raw.unwrap_or("ui_full").to_ascii_lowercase().as_str() {
        "tool_compact" => ProvenanceResponseProfile::ToolCompact,
        _ => ProvenanceResponseProfile::UiFull,
    }
}

fn to_ops_request(
    resource: ProvenanceOpsResource,
    q: ProvenanceQueryParams,
) -> ProvenanceOpsQueryRequest {
    let group_by = q
        .group_by
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ProvenanceOpsQueryRequest {
        resource,
        filters: ProvenanceOpsFilters {
            context_id: q.context_id,
            task_id: q.task_id,
            agent_id: q.agent_id,
            provider: q.provider,
            model: q.model,
            tool_name: q.tool_name,
            baml_prompt: q.baml_prompt,
            payload_text: q.payload_text,
            from_timestamp_ms: q.from_timestamp_ms,
            to_timestamp_ms: q.to_timestamp_ms,
        },
        group_by,
        sort_by: q.sort_by,
        sort_dir: q.sort_dir,
        page_size: q.page_size,
        cursor: q.cursor,
        top_k: q.top_k,
        outcome: Some(parse_outcome(q.outcome.as_deref())),
        response_profile: Some(parse_profile(q.response_profile.as_deref())),
        budget_mode: true,
    }
}

async fn run_ops_query(
    state: &Arc<ApiState>,
    route: &str,
    resource: ProvenanceOpsResource,
    query: ProvenanceQueryParams,
    start: Instant,
) -> HttpResult<Json<Value>> {
    let Some(svc) = &state.provenance_ops else {
        metrics::record_request(route, "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Provenance query service not configured",
        ));
    };
    let req = to_ops_request(resource, query);
    match svc.query(req).await {
        Ok(report) => {
            metrics::record_request(route, "success", start.elapsed());
            let value = serde_json::to_value(report).map_err(|e| {
                problem(
                    500,
                    "Internal Server Error",
                    format!("serialization failed: {e}"),
                )
            })?;
            Ok(Json(value))
        }
        Err(ProvenanceOpsError::NotFound) => {
            metrics::record_request(route, "not_found", start.elapsed());
            Err(problem(404, "Not Found", "No provenance rows for query"))
        }
        Err(ProvenanceOpsError::Unavailable) => {
            metrics::record_request(route, "unavailable", start.elapsed());
            Err(problem(
                501,
                "Not Implemented",
                "Provenance query service unavailable",
            ))
        }
        Err(ProvenanceOpsError::Other(e)) => {
            metrics::record_request(route, "internal", start.elapsed());
            Err(problem(500, "Internal Server Error", e.to_string()))
        }
    }
}

#[utoipa::path(
    get,
    path = "/provenance/llm-calls",
    tag = "provenance",
    responses((status = 200, description = "Provenance LLM calls query response", body = Value))
)]
pub async fn get_provenance_llm_calls(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProvenanceQueryParams>,
) -> HttpResult<Json<Value>> {
    let start = Instant::now();
    let span = spans::get_provenance_llm_calls();
    let _guard = span.enter();
    run_ops_query(
        &state,
        "get_provenance_llm_calls",
        ProvenanceOpsResource::LlmCalls,
        query,
        start,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/provenance/tool-calls",
    tag = "provenance",
    responses((status = 200, description = "Provenance tool calls query response", body = Value))
)]
pub async fn get_provenance_tool_calls(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProvenanceQueryParams>,
) -> HttpResult<Json<Value>> {
    let start = Instant::now();
    let span = spans::get_provenance_tool_calls();
    let _guard = span.enter();
    run_ops_query(
        &state,
        "get_provenance_tool_calls",
        ProvenanceOpsResource::ToolCalls,
        query,
        start,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/provenance/messages",
    tag = "provenance",
    summary = "Message analytics query (not transcript reconstruction)",
    description = "Analytics-only message aggregates/dimensions. For UI chat/provenance observation, use GET /contexts/{context_id}/conversation-history and /contexts/{context_id}/conversation-history/stream.",
    responses((status = 200, description = "Provenance message query response", body = Value))
)]
pub async fn get_provenance_messages(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProvenanceQueryParams>,
) -> HttpResult<Json<Value>> {
    let start = Instant::now();
    let span = spans::get_provenance_messages();
    let _guard = span.enter();
    run_ops_query(
        &state,
        "get_provenance_messages",
        ProvenanceOpsResource::Messages,
        query,
        start,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/provenance/aggregates",
    tag = "provenance",
    responses((status = 200, description = "Provenance aggregate query response", body = Value))
)]
pub async fn get_provenance_aggregates(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProvenanceQueryParams>,
) -> HttpResult<Json<Value>> {
    let start = Instant::now();
    let span = spans::get_provenance_aggregates();
    let _guard = span.enter();
    run_ops_query(
        &state,
        "get_provenance_aggregates",
        ProvenanceOpsResource::Aggregates,
        query,
        start,
    )
    .await
}

/// Query lifecycle events (agent boot/stop) from provenance.
#[utoipa::path(
    get,
    path = "/provenance/lifecycle-events",
    tag = "provenance",
    responses((status = 200, description = "Provenance lifecycle events query response", body = Value))
)]
pub async fn get_provenance_lifecycle_events(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ProvenanceQueryParams>,
) -> HttpResult<Json<Value>> {
    let start = Instant::now();
    let span = spans::get_provenance_lifecycle_events();
    let _guard = span.enter();
    run_ops_query(
        &state,
        "get_provenance_lifecycle_events",
        ProvenanceOpsResource::LifecycleEvents,
        query,
        start,
    )
    .await
}

fn domain_to_problem(
    e: &BamlRtError,
    agent_package: &str,
    agent_instance_id: &str,
) -> HttpApiProblem {
    let (status, title, detail) = match e {
        BamlRtError::AgentNotFound(_) => (
            404u16,
            "Not Found",
            format!("Agent {agent_package}/{agent_instance_id} not found"),
        ),
        BamlRtError::Conflict(msg) => (409u16, "Conflict", msg.clone()),
        BamlRtError::InvalidArgument(msg) => (400u16, "Bad Request", msg.clone()),
        BamlRtError::SessionLifecycle(
            baml_rt_core::SessionLifecycleError::ToolSessionNotFound { session_id },
        ) => (
            404u16,
            "Not Found",
            format!("Tool session not found: {session_id}"),
        ),
        BamlRtError::SessionLifecycle(
            baml_rt_core::SessionLifecycleError::StreamSessionNotFound { stream_session_id },
        ) => (
            404u16,
            "Not Found",
            format!("Stream session not found: {stream_session_id}"),
        ),
        BamlRtError::SessionLifecycle(e) => (400u16, "Bad Request", e.to_string()),
        BamlRtError::FunctionNotFound(msg) => (404u16, "Not Found", msg.clone()),
        _ => {
            tracing::warn!(
                error = ?e,
                agent_package = %agent_package,
                agent_instance_id = %agent_instance_id,
                "Unmapped BamlRtError converted to 500"
            );
            (500u16, "Internal Server Error", e.to_string())
        }
    };
    problem(status, title, detail)
}

/// Deterministic host-to-agent delivery: POST /agents/{agent_package}/{agent_instance_id}/dispatch
#[utoipa::path(
    post,
    path = "/agents/{agent_package}/{agent_instance_id}/dispatch",
    tag = "agents",
    params(
        ("agent_package" = String, Path, description = "Agent package name"),
        ("agent_instance_id" = String, Path, description = "Agent instance ID"),
    ),
    request_body = crate::openapi::AgentDispatchRequestDto,
    responses(
        (status = 200, description = "Delivery accepted", body = crate::openapi::AgentDispatchAckDto),
        (status = 400, description = "Bad request (validation failed)"),
        (status = 404, description = "Agent not found"),
    )
)]
pub async fn post_dispatch(
    State(state): State<Arc<crate::router::ApiState>>,
    axum::extract::Path((agent_package, agent_instance_id)): axum::extract::Path<(String, String)>,
    Json(body): Json<crate::openapi::AgentDispatchRequestDto>,
) -> impl IntoResponse {
    let span = spans::post_dispatch(&agent_package, &agent_instance_id);
    let _guard = span.enter();

    let package_name = match AgentPackageName::parse(&agent_package) {
        Some(n) => n,
        None => {
            return (
                AxumStatus::BAD_REQUEST,
                format!("invalid agent_package: {agent_package}"),
            )
                .into_response();
        }
    };
    let instance_id = match AgentInstanceId::parse(&agent_instance_id) {
        Some(i) => i,
        None => {
            return (
                AxumStatus::BAD_REQUEST,
                format!("invalid agent_instance_id: {agent_instance_id}"),
            )
                .into_response();
        }
    };
    let key = AgentRouteKey::new(package_name, instance_id);
    let request: AgentDispatchRequest = match body.try_into() {
        Ok(r) => r,
        Err(e) => return (AxumStatus::BAD_REQUEST, e.to_string()).into_response(),
    };

    match state.registry.handle_dispatch(&key, request).await {
        Ok(ack) => {
            let dto: crate::openapi::AgentDispatchAckDto = ack.into();
            Json(dto).into_response()
        }
        Err(e) => domain_to_problem(&e, &agent_package, &agent_instance_id).into_response(),
    }
}

/// Get a one-shot episode snapshot for a completed or in-progress task.
#[utoipa::path(
    get,
    path = "/tasks/{task_id}/episode",
    tag = "provenance",
    summary = "Episode snapshot by task_id",
    params(("task_id" = String, Path, description = "A2A task ID")),
    responses(
        (status = 200, description = "Episode snapshot", body = EpisodeSnapshotDto),
        (status = 404, description = "No episode found for task"),
        (status = 501, description = "Episode service not available"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_episode(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> HttpResult<Json<EpisodeSnapshotDto>> {
    let span = spans::get_episode(&task_id);
    let _guard = span.enter();
    let start = Instant::now();
    let Some(svc) = &state.episode else {
        metrics::record_request("get_episode", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Episode service not configured",
        ));
    };
    service_result_to_http("get_episode", start, svc.episode_snapshot(&task_id).await)
}

/// Download the canonical text rendering of an episode (produced by `render_episode`).
#[utoipa::path(
    get,
    path = "/tasks/{task_id}/episode/text",
    tag = "provenance",
    summary = "Episode as canonical plain text",
    params(("task_id" = String, Path, description = "A2A task ID")),
    responses(
        (status = 200, description = "Episode plain text", content_type = "text/plain"),
        (status = 404, description = "No episode found for task"),
        (status = 501, description = "Episode service not available"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_episode_text(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, HttpApiProblem> {
    let start = Instant::now();
    let Some(svc) = &state.episode else {
        metrics::record_request("get_episode_text", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Episode service not configured",
        ));
    };
    match svc.episode_text(&task_id).await {
        Ok(text) => {
            metrics::record_request("get_episode_text", "success", start.elapsed());
            let filename = format!("episode-{task_id}.txt");
            Ok((
                [
                    ("content-type", "text/plain; charset=utf-8".to_string()),
                    (
                        "content-disposition",
                        format!("attachment; filename=\"{filename}\""),
                    ),
                ],
                text,
            ))
        }
        Err(e) => {
            let resp =
                service_result_to_http::<EpisodeSnapshotDto>("get_episode_text", start, Err(e));
            Err(resp.unwrap_err())
        }
    }
}

/// SSE streaming episode updates for a task.
#[utoipa::path(
    get,
    path = "/tasks/{task_id}/episode/stream",
    tag = "provenance",
    summary = "Streaming episode updates by task_id",
    params(("task_id" = String, Path, description = "A2A task ID")),
    responses(
        (status = 200, description = "SSE stream of episode snapshots", content_type = "text/event-stream"),
        (status = 404, description = "No episode found for task"),
        (status = 501, description = "Episode service not available"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_episode_stream(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> HttpResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let start = Instant::now();
    let Some(svc) = &state.episode else {
        metrics::record_request("get_episode_stream", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Episode service not configured",
        ));
    };
    let initial = match service_result_to_http(
        "get_episode_stream",
        start,
        svc.episode_snapshot(&task_id).await,
    ) {
        Ok(axum::Json(s)) => s,
        Err(e) => return Err(e),
    };
    let svc = Arc::clone(svc);
    let stream = async_stream::stream! {
        let data = serde_json::to_string(&initial).unwrap_or_else(|_| "{}".into());
        yield Ok::<_, Infallible>(Event::default().event("snapshot").data(data));
        if initial.status.is_terminal() {
            let data = serde_json::to_string(&initial).unwrap_or_else(|_| "{}".into());
            yield Ok::<_, Infallible>(Event::default().event("done").data(data));
            return;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        let mut prev_transcript_len = initial.transcript.len();
        let mut prev_session_history_len = initial.session_history.len();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if std::time::Instant::now() > deadline { break; }
            match svc.episode_snapshot(&task_id).await {
                Ok(snap) => {
                    let changed = snap.transcript.len() != prev_transcript_len
                        || snap.session_history.len() != prev_session_history_len
                        || snap.status.is_terminal();
                    if !changed { continue; }
                    prev_transcript_len = snap.transcript.len();
                    prev_session_history_len = snap.session_history.len();
                    let terminal = snap.status.is_terminal();
                    let data = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
                    yield Ok::<_, Infallible>(Event::default().event("snapshot").data(data));
                    if terminal {
                        let data = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
                        yield Ok::<_, Infallible>(Event::default().event("done").data(data));
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(task_id = %task_id, error = %e, "Episode snapshot failed during stream");
                }
            }
        }
    };
    metrics::record_request("get_episode_stream", "success", start.elapsed());
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("")))
}

fn parse_conversation_history_request(
    context_id: &str,
    query: ConversationHistoryQueryParams,
) -> Result<ConversationHistoryRequest, HttpApiProblem> {
    ConversationHistoryRequest::from_parts(context_id, query)
        .map_err(|e: ConversationHistoryRequestParseError| bad_request_problem(e.to_string()))
}

fn parse_context_index_request(
    query: ContextIndexQueryParams,
) -> Result<ContextIndexRequest, HttpApiProblem> {
    ContextIndexRequest::from_query(query)
        .map_err(|e: ContextIndexRequestParseError| bad_request_problem(e.to_string()))
}

/// Context-scoped picker index for chat history restore.
#[utoipa::path(
    get,
    path = "/contexts",
    tag = "provenance",
    summary = "Context picker index",
    description = "Typed context index for chat history selection. This is the product workflow source for context-scoped restore.",
    params(
        ("agentPackage" = Option<String>, Query, description = "Optional agent package filter"),
        ("limit" = Option<u32>, Query, description = "Page size in range [1, 200], default 50"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor")
    ),
    responses(
        (status = 200, description = "Context picker page", body = ContextPickerPageDto),
        (status = 400, description = "Invalid query/cursor"),
        (status = 501, description = "Context index service not configured"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_context_index(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ContextIndexQueryParams>,
) -> HttpResult<Json<ContextPickerPageDto>> {
    let span = spans::get_context_index();
    let _guard = span.enter();
    let start = Instant::now();
    let req = parse_context_index_request(query)?;
    let Some(svc) = &state.context_index else {
        metrics::record_request("get_context_index", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Context index service not configured",
        ));
    };
    service_result_to_http("get_context_index", start, svc.page(&req).await)
}

/// Get normalized provenance conversation history for a context (optionally scoped to a task).
#[utoipa::path(
    get,
    path = "/contexts/{context_id}/conversation-history",
    tag = "provenance",
    summary = "Conversation history page by context_id",
    params(
        ("context_id" = String, Path, description = "Provenance context ID"),
        ("taskId" = Option<String>, Query, description = "Optional task scope"),
        ("limit" = Option<u32>, Query, description = "Page size in range [1, 500], default 100"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor"),
        ("profile" = Option<String>, Query, description = "Payload profile: full or compact"),
        ("format" = Option<String>, Query, description = "Response format: full")
    ),
    responses(
        (status = 200, description = "Conversation history page", body = ConversationHistoryPageDto),
        (status = 400, description = "Invalid context/query/cursor"),
        (status = 501, description = "Conversation history service not configured"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_conversation_history(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(context_id): axum::extract::Path<String>,
    Query(query): Query<ConversationHistoryQueryParams>,
) -> HttpResult<Json<ConversationHistoryPageDto>> {
    let span = spans::get_conversation_history(&context_id);
    let _guard = span.enter();
    let start = Instant::now();
    let req = parse_conversation_history_request(&context_id, query)?;
    let Some(svc) = &state.conversation_history else {
        metrics::record_request("get_conversation_history", "unavailable", start.elapsed());
        return Err(problem(
            501,
            "Not Implemented",
            "Conversation history service not configured",
        ));
    };
    service_result_to_http("get_conversation_history", start, svc.page(&req).await)
}

/// SSE stream of normalized conversation snapshots for one context scope.
///
/// Event source is provenance-normalized reads, not A2A stream relays.
#[utoipa::path(
    get,
    path = "/contexts/{context_id}/conversation-history/stream",
    tag = "provenance",
    summary = "Streaming conversation history updates by context_id",
    params(
        ("context_id" = String, Path, description = "Provenance context ID"),
        ("taskId" = Option<String>, Query, description = "Optional task scope"),
        ("limit" = Option<u32>, Query, description = "Page size in range [1, 500], default 100"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor"),
        ("profile" = Option<String>, Query, description = "Payload profile: full or compact"),
        ("format" = Option<String>, Query, description = "Response format: full")
    ),
    responses(
        (status = 200, description = "SSE stream of conversation history snapshots", content_type = "text/event-stream"),
        (status = 400, description = "Invalid context/query/cursor"),
        (status = 501, description = "Conversation history service not configured"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn get_conversation_history_stream(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(context_id): axum::extract::Path<String>,
    Query(query): Query<ConversationHistoryQueryParams>,
) -> HttpResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let span = spans::get_conversation_history_stream(&context_id);
    let _guard = span.enter();
    let start = Instant::now();
    let req = parse_conversation_history_request(&context_id, query)?;
    let Some(svc) = &state.conversation_history else {
        metrics::record_request(
            "get_conversation_history_stream",
            "unavailable",
            start.elapsed(),
        );
        return Err(problem(
            501,
            "Not Implemented",
            "Conversation history service not configured",
        ));
    };
    let Some(event_svc) = &state.conversation_history_events else {
        metrics::record_request(
            "get_conversation_history_stream",
            "unavailable",
            start.elapsed(),
        );
        return Err(problem(
            501,
            "Not Implemented",
            "Conversation history event service not configured",
        ));
    };
    let initial_query_start = Instant::now();
    let initial = match service_result_to_http(
        "get_conversation_history_stream",
        start,
        svc.page(&req).await,
    ) {
        Ok(axum::Json(s)) => s,
        Err(e) => return Err(e),
    };
    metrics::record_conversation_history_phase_duration(
        "initial_query",
        initial_query_start.elapsed(),
    );

    let svc = Arc::clone(svc);
    let request = req.clone();
    let limit = request.page.limit();
    let mut updates = event_svc.subscribe_updates();
    let stream = async_stream::stream! {
        let mut latest = initial.clone();
        let mut last_event_order = latest.max_event_order;
        let mut last_version = latest.version.clone();
        let serialize_start = Instant::now();
        let data = serde_json::to_string(&initial).unwrap_or_else(|_| "{}".into());
        metrics::record_conversation_history_phase_duration("serialize_snapshot", serialize_start.elapsed());
        metrics::record_conversation_history_payload("snapshot", data.len(), initial.items.len());
        yield Ok::<_, Infallible>(Event::default().event("snapshot").data(data));

        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                let data = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".into());
                yield Ok::<_, Infallible>(Event::default().event("done").data(data));
                break;
            }
            let wait_budget = deadline.saturating_duration_since(now);
            let should_refresh = match tokio::time::timeout(wait_budget, updates.recv()).await {
                Ok(Ok(update)) => {
                    if update.context_id != request.context_id.as_str() {
                        false
                    } else if let Some(req_task_id) = request.task_id.as_ref().map(TaskId::as_str) {
                        update.task_id.as_deref() == Some(req_task_id)
                    } else {
                        true
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => true,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    let data = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".into());
                    yield Ok::<_, Infallible>(Event::default().event("done").data(data));
                    break;
                }
                Err(_) => {
                    let data = serde_json::to_string(&latest).unwrap_or_else(|_| "{}".into());
                    yield Ok::<_, Infallible>(Event::default().event("done").data(data));
                    break;
                }
            };
            if !should_refresh {
                continue;
            }
            let delta_req = ConversationHistoryDeltaRequest {
                context_id: request.context_id.clone(),
                task_id: request.task_id.clone(),
                after_event_order: last_event_order,
                limit,
                profile: request.profile,
                format: request.format,
            };

            let delta_query_start = Instant::now();
            match svc.delta_after_event_order(&delta_req).await {
                Ok(page) => {
                    metrics::record_conversation_history_phase_duration(
                        "delta_query",
                        delta_query_start.elapsed(),
                    );
                    if page.items.is_empty() {
                        continue;
                    }
                    if page.version == last_version {
                        continue;
                    }
                    latest.items.extend(page.items.clone());
                    latest.max_event_order = page.max_event_order.max(latest.max_event_order);
                    latest.version = page.version.clone();
                    last_event_order = latest.max_event_order;
                    last_version = latest.version.clone();
                    let serialize_start = Instant::now();
                    let data = serde_json::to_string(&page).unwrap_or_else(|_| "{}".into());
                    metrics::record_conversation_history_phase_duration(
                        "serialize_delta",
                        serialize_start.elapsed(),
                    );
                    metrics::record_conversation_history_payload("delta", data.len(), page.items.len());
                    yield Ok::<_, Infallible>(Event::default().event("delta").data(data));
                }
                Err(e) => {
                    tracing::warn!(
                        context_id = %request.context_id,
                        error = %e,
                        "Conversation history snapshot failed during stream"
                    );
                }
            }
        }
    };
    metrics::record_request(
        "get_conversation_history_stream",
        "success",
        start.elapsed(),
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("")))
}
