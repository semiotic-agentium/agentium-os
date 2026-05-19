//! HTTP handlers for Event Console routes.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use baml_rt_core::{AgentInstanceId, AgentPackageName, AgentRouteKey};

use crate::{
    event_console::{
        registry::registry_response,
        types::{
            EventDispatchValidateRequestDto, EventValidationReportDto, MessageShapeRegistryResponse,
        },
        validation::validate_draft,
    },
    router::ApiState,
};

/// Agent-deliverable message shapes for the operator Event Console.
#[utoipa::path(
    get,
    path = "/message-shapes",
    tag = "events",
    responses((status = 200, description = "Message shape registry", body = MessageShapeRegistryResponse))
)]
pub async fn get_message_shapes(
    State(_state): State<Arc<ApiState>>,
) -> Json<MessageShapeRegistryResponse> {
    Json(registry_response())
}

/// Validate an event dispatch draft without invoking the agent.
#[utoipa::path(
    post,
    path = "/event-dispatch/validate",
    tag = "events",
    request_body = EventDispatchValidateRequestDto,
    responses((status = 200, description = "Validation report", body = EventValidationReportDto))
)]
pub async fn post_event_dispatch_validate(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<EventDispatchValidateRequestDto>,
) -> Json<EventValidationReportDto> {
    Json(validate_draft(state.registry.as_ref(), &body))
}

pub fn parse_route_key(
    agent_package: &str,
    agent_instance_id: &str,
) -> Result<AgentRouteKey, (StatusCode, String)> {
    let package = AgentPackageName::parse(agent_package).ok_or((
        StatusCode::BAD_REQUEST,
        "agent_package must match [A-Za-z0-9_-]".to_string(),
    ))?;
    let instance = AgentInstanceId::parse(agent_instance_id).ok_or((
        StatusCode::BAD_REQUEST,
        "agent_instance_id must match [A-Za-z0-9_-]".to_string(),
    ))?;
    Ok(AgentRouteKey::new(package, instance))
}
