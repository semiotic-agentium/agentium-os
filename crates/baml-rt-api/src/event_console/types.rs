//! Shared types for the Event Console HTTP surface.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageShapeFieldGroup {
    pub title: String,
    pub json_pointers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, ToSchema)]
pub struct MessageShapeUiHints {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub field_labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub field_descriptions: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_groups: Vec<MessageShapeFieldGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_record_array_pointer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageShapeDeliveryDefaults {
    pub routing_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageShapeSample {
    pub sample_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    pub payload: Value,
}

/// Agent-deliverable message shape: one JSON body operators can put in `messages[]`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentDeliverableMessageShape {
    pub message_shape_id: String,
    pub display_name: String,
    pub description: String,
    /// Required origin identity (tool path or daemon name) for traceability.
    pub origin: String,
    pub payload_name: String,
    /// Wire-level schema version (`AgentDispatchRequest.message_type`).
    pub wire_schema_version: String,
    /// Default `source_kind` for delivery envelope derivation.
    pub source_kind: String,
    pub payload_schema: Value,
    pub samples: Vec<MessageShapeSample>,
    pub delivery_defaults: MessageShapeDeliveryDefaults,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_hints: Option<MessageShapeUiHints>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageShapeRegistryResponse {
    pub items: Vec<AgentDeliverableMessageShape>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventDispatchScopeDto {
    NewContext,
    ExistingContext { context_id: String },
    ExistingTask { context_id: String, task_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EventDispatchValidateRequestDto {
    pub agent_package: String,
    pub agent_instance_id: String,
    pub routing_key: String,
    /// Wire-level message type (`AgentDispatchRequest.message_type`).
    pub message_type: String,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_key: Option<String>,
    pub messages: Vec<Value>,
    pub scope: EventDispatchScopeDto,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EventValidationIssueDto {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_pointer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EventValidationReportDto {
    pub valid: bool,
    pub matched_subscription: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<EventValidationIssueDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<EventValidationIssueDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_produced_event: Option<Value>,
}
