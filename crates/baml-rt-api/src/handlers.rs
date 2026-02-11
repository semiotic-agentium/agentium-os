//! HTTP handlers: discovery and A2A forward (POST and SSE).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode as AxumStatus;
use axum::response::sse::{Event, KeepAlive, Sse};
use baml_rt_core::AgentRouteKey;
use baml_rt_core::BamlRtError;
use futures_util::stream::{self, Stream};
use http_api_problem::HttpApiProblem;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ApiState;
use crate::metrics;
use crate::openapi::AgentDiscoveryEntryDto;
use crate::spans;

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
    let key = AgentRouteKey::new(agent_package.clone(), agent_instance_id.clone());

    if !body.is_object() {
        metrics::record_request("post_a2a", "error", start.elapsed());
        return Err(problem(
            400,
            "Bad Request",
            "Body must be a JSON object (JSON-RPC request)",
        ));
    }

    match state.registry.handle_a2a(&key, body).await {
        Ok(responses) => {
            metrics::record_request("post_a2a", "success", start.elapsed());
            Ok(Json(responses))
        }
        Err(e) => {
            metrics::record_request("post_a2a", "error", start.elapsed());
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
        (status = 404, description = "Agent not found"),
        (status = 500, description = "Internal error")
    )
)]
pub async fn post_a2a_sse(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((agent_package, agent_instance_id)): axum::extract::Path<(String, String)>,
    Json(body): Json<Value>,
) -> HttpResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let span = spans::post_a2a_sse(&agent_package, &agent_instance_id);
    let _guard = span.enter();
    let start = Instant::now();
    let key = AgentRouteKey::new(agent_package.clone(), agent_instance_id.clone());

    if !body.is_object() {
        metrics::record_request("post_a2a_sse", "error", start.elapsed());
        return Err(problem(
            400,
            "Bad Request",
            "Body must be a JSON object (JSON-RPC request)",
        ));
    }

    let responses = match state.registry.handle_a2a(&key, body).await {
        Ok(r) => r,
        Err(e) => {
            metrics::record_request("post_a2a_sse", "error", start.elapsed());
            return Err(domain_to_problem(&e, &agent_package, &agent_instance_id));
        }
    };

    let data_strings: Result<Vec<String>, HttpApiProblem> = responses
        .into_iter()
        .map(|v| {
            serde_json::to_string(&v)
                .map_err(|e| problem(500, "Internal Server Error", format!("Serialization: {e}")))
        })
        .collect();
    let data_strings = match data_strings {
        Ok(d) => d,
        Err(e) => {
            metrics::record_request("post_a2a_sse", "error", start.elapsed());
            return Err(e);
        }
    };
    let stream = stream::iter(
        data_strings
            .into_iter()
            .map(|data| Ok(Event::default().data(data))),
    );

    let sse =
        Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text(""));

    metrics::record_request("post_a2a_sse", "success", start.elapsed());
    Ok(sse)
}

fn domain_to_problem(
    e: &BamlRtError,
    agent_package: &str,
    agent_instance_id: &str,
) -> HttpApiProblem {
    let (status, title, detail) = match e {
        BamlRtError::InvalidArgument(msg) if msg.contains("not found") => (
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
