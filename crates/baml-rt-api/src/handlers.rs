//! HTTP handlers: discovery and A2A forward (POST and SSE).

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
    A2aWireRequest, AgentInstanceId, AgentPackageName, AgentRouteKey, BamlRtError,
    collect_a2a_stream,
    ids::{AgentId, ContextId, TaskId},
};
use baml_rt_provenance::{
    ProvenanceOpsFilters, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    ProvenanceOutcomeSegment, ProvenanceResponseProfile,
};
use futures_util::stream::{Stream, StreamExt};
use http_api_problem::HttpApiProblem;
use serde_json::Value;

use crate::{
    ApiState,
    context_metrics::{ContextMetricsError, ContextMetricsResponseDto},
    mermaid::MermaidError,
    metrics,
    openapi::AgentDiscoveryEntryDto,
    planning::{ContextPlanningResponse, PlanningError},
    provenance_ops::ProvenanceOpsError,
    spans,
};

/// HTTP result type for handlers that return RFC 7807 problem details on error.
type HttpResult<T> = Result<T, HttpApiProblem>;

/// Build HttpApiProblem for known-valid status codes (400, 404, 409, 500 per RFC 7231).
/// Caller must only pass these codes; unwrap is justified here with this invariant.
fn problem(status: u16, title: &str, detail: impl Into<String>) -> HttpApiProblem {
    HttpApiProblem::try_new(status)
        .expect("400, 404, 500 are valid HTTP status codes")
        .title(title)
        .detail(detail)
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

/// Forward A2A JSON-RPC request (POST /agents/{agent_package}/{agent_instance_id}/a2a).
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

/// A2A over Server-Sent Events: POST /agents/{agent_package}/{agent_instance_id}/a2a/sse
/// Request body: JSON-RPC request. Response: Content-Type: text/event-stream; each event's
/// `data` is one JSON-RPC response. Keep-alive comments sent on interval while stream is open.
#[utoipa::path(
    post,
    path = "/agents/{agent_package}/{agent_instance_id}/a2a/sse",
    tag = "agents",
    params(
        ("agent_package" = String, Path, description = "Agent package identifier"),
        ("agent_instance_id" = String, Path, description = "Agent instance identifier")
    ),
    request_body = Value,
    responses(
        (status = 200, description = "SSE stream of JSON-RPC responses", content_type = "text/event-stream"),
        (status = 400, description = "Malformed request"),
        (status = 409, description = "Conflicting concurrent stream request"),
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn post_a2a_sse(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((agent_package, agent_instance_id)): axum::extract::Path<(String, String)>,
    Json(body): Json<Value>,
) -> HttpResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    tracing::info!(
        agent_package = %agent_package,
        agent_instance_id = %agent_instance_id,
        "A2A SSE request received"
    );
    let span = spans::post_a2a_sse(&agent_package, &agent_instance_id);
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
        metrics::record_request("post_a2a_sse", "bad_request", start.elapsed());
        return Err(problem(
            400,
            "Bad Request",
            "Body must be a JSON object (JSON-RPC request)",
        ));
    }

    tracing::debug!(%agent_package, "A2A SSE: calling handle_a2a_stream");
    let stream = match state
        .registry
        .handle_a2a_stream(&key, A2aWireRequest::from(body))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            metrics::record_request(
                "post_a2a_sse",
                result_label_for_domain_error(&e),
                start.elapsed(),
            );
            return Err(domain_to_problem(&e, &agent_package, &agent_instance_id));
        }
    };
    tracing::info!(%agent_package, "A2A SSE: stream obtained, forwarding incrementally");
    let event_stream = stream.map(|chunk| {
        let data = serde_json::to_string(chunk.as_ref()).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "SSE chunk serialization failed");
            r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"Chunk serialization failed"}}"#
                .to_string()
        });
        Ok::<_, Infallible>(Event::default().data(data))
    });

    let sse = Sse::new(event_stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text(""));

    metrics::record_request("post_a2a_sse", "success", start.elapsed());
    Ok(sse)
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
            Ok(Json(serde_json::to_value(report).unwrap_or(Value::Null)))
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
