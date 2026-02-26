//! HTTP handlers: discovery and A2A forward (POST and SSE).

use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::State,
    http::StatusCode as AxumStatus,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey, BamlRtError};
use futures_util::stream::{Stream, StreamExt};
use http_api_problem::HttpApiProblem;
use serde_json::Value;

use crate::{ApiState, mermaid::MermaidError, metrics, openapi::AgentDiscoveryEntryDto, spans};

/// HTTP result type for handlers that return RFC 7807 problem details on error.
type HttpResult<T> = Result<T, HttpApiProblem>;

/// Build HttpApiProblem for known-valid status codes (400, 404, 500 per RFC 7231).
/// Caller must only pass these codes; unwrap is justified here with this invariant.
fn problem(status: u16, title: &str, detail: impl Into<String>) -> HttpApiProblem {
    HttpApiProblem::try_new(status)
        .expect("400, 404, 500 are valid HTTP status codes")
        .title(title)
        .detail(detail)
}

fn result_label_for_domain_error(error: &BamlRtError) -> &'static str {
    match error {
        BamlRtError::InvalidArgument(msg) if msg.contains("not found") => "not_found",
        BamlRtError::InvalidArgument(_) => "bad_request",
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
        .handle_a2a_stream(&key, baml_rt_core::A2aWireRequest::from(body))
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
    tracing::info!(%agent_package, "A2A SSE: stream obtained, forwarding as SSE");
    let event_stream = stream.map(|chunk: baml_rt_core::A2aStreamChunk| {
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
    path = "/mermaid/context/{context_id}",
    tag = "mermaid",
    summary = "Mermaid diagram by context",
    description = "Returns the provenance subgraph for the given A2A context ID as a Mermaid sequenceDiagram (text/plain). Available when the runner is started with GraphQLite provenance.",
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
    path = "/mermaid/task/{task_id}",
    tag = "mermaid",
    summary = "Mermaid diagram by task",
    description = "Returns the provenance subgraph for the given A2A task ID as a Mermaid sequenceDiagram (text/plain). Available when the runner is started with GraphQLite provenance.",
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
        BamlRtError::InvalidArgument(msg) => (400u16, "Bad Request", msg.clone()),
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
