// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Structured request and response types for system tools.

use baml_derive::BamlType;
use baml_derive_core::{JsonSchemaType, TsType};
use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use baml_rt_tools::{
    OpaqueJson,
    tools::{HistoryContextV1, SessionReadEnvelope, SessionReadMode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct InternalA2aTarget {
    pub agent_package: String,
    pub agent_instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct InternalA2aOpenInput {
    pub target: InternalA2aTarget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(description = "Plain text content for this conversation part.")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Raw provider-native content when plain text would be lossy or unavailable."
    )]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Remote URL for this part when the content points to a web or attachment resource."
    )]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(description = "Attachment filename when this part refers to a file.")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(description = "MIME type for this part, for example text/plain or image/png.")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
/// Message sent to a remote agent.
/// Provide at least one part with text content.
pub struct InternalA2aSendInput {
    /// Message parts. Provide at least one with a non-empty text field.
    pub parts: Vec<ConversationPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<ConversationPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ConversationChunk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<ConversationMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_update: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_update: Option<String>,
}

/// Completion reason for internal_a2a stream. When INPUT_REQUIRED, the delegated agent
/// suspended for input; caller can resume with a new Send + Read using the same context_id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InternalA2aCompletion {
    #[default]
    Done,
    InputRequired,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct InternalA2aNextOutput {
    pub chunks: Vec<ConversationChunk>,
    /// Present when the delegated agent paused and needs more input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<InternalA2aCompletion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

// --- system/callback ---

fn tagged_union_variant_schema<T: JsonSchemaType>(tag_name: &str, tag_value: &str) -> Value {
    let mut schema = match T::json_schema_inline() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };

    schema.insert("type".to_string(), Value::String("object".to_string()));

    let mut properties = match schema.remove("properties") {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    properties.insert(
        tag_name.to_string(),
        json!({
            "type": "string",
            "const": tag_value,
        }),
    );
    schema.insert("properties".to_string(), Value::Object(properties));

    let mut required = match schema.remove("required") {
        Some(Value::Array(values)) => values,
        _ => Vec::new(),
    };
    let tag_name_value = Value::String(tag_name.to_string());
    if !required.iter().any(|value| value == &tag_name_value) {
        required.push(tag_name_value);
    }
    schema.insert("required".to_string(), Value::Array(required));

    Value::Object(schema)
}

fn tagged_union_schema(variants: Vec<Value>) -> Value {
    json!({ "oneOf": variants })
}

fn tagged_union_ts_decl(
    type_name: &str,
    tag_name: &str,
    variants: &[(&str, &str)],
) -> Option<String> {
    let union = variants
        .iter()
        .map(|(tag_value, inner_type)| {
            format!("({{ {tag_name}: \"{tag_value}\" }} & {inner_type})")
        })
        .collect::<Vec<_>>()
        .join(" | ");
    Some(format!("export type {type_name} = {union};"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CallbackToolInput {
    Schedule(CallbackScheduleInput),
    Cancel(CallbackCancelInput),
}

impl TsType for CallbackToolInput {
    fn ts_type_name() -> &'static str {
        "CallbackToolInput"
    }

    fn ts_decl() -> Option<String> {
        tagged_union_ts_decl(
            Self::ts_type_name(),
            "op",
            &[
                ("schedule", "CallbackScheduleInput"),
                ("cancel", "CallbackCancelInput"),
            ],
        )
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["CallbackScheduleInput", "CallbackCancelInput"]
    }
}

impl JsonSchemaType for CallbackToolInput {
    fn json_schema_inline() -> Value {
        tagged_union_schema(vec![
            tagged_union_variant_schema::<CallbackScheduleInput>("op", "schedule"),
            tagged_union_variant_schema::<CallbackCancelInput>("op", "cancel"),
        ])
    }
}

impl baml_rt_tools::DescribeAction for CallbackToolInput {
    fn describe(&self) -> String {
        match self {
            Self::Schedule(input) => input.describe(),
            Self::Cancel(input) => input.describe(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct CallbackScheduleInput {
    #[serde(alias = "after_ms")]
    #[baml(description = "Delay before the callback event is emitted, in milliseconds.")]
    pub after_ms: u64,
    #[serde(alias = "source_key")]
    #[baml(
        description = "Stable event source key for subscription matching, for example `coordinator-agent:follow-up`."
    )]
    pub source_key: String,
    #[baml(description = "Opaque JSON payload delivered back through onDispatch.")]
    pub payload: OpaqueJson,
    #[serde(alias = "dedupe_key", skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Optional idempotency key scoped to the sourceKey. A pending callback with the same sourceKey + dedupeKey is reused instead of creating another row."
    )]
    pub dedupe_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Optional continuation policy. Omit it for a detached callback. Use `resume_current_task` to re-enter the current task later in the same context after the current turn has quiesced."
    )]
    pub continuation: Option<CallbackContinuationMode>,
}

impl baml_rt_tools::DescribeAction for CallbackScheduleInput {
    fn describe(&self) -> String {
        format!(
            "scheduling callback '{source_key}' in {after_ms} ms",
            source_key = self.source_key,
            after_ms = self.after_ms
        )
    }
}

/// Continuation mode for `system/callback`.
///
/// Canonical wire values are snake_case for serde and runtime storage.
/// PascalCase serde aliases keep older callers working, and BAML aliases keep
/// generated BAML surfaces aligned with the snake_case wire contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackContinuationMode {
    #[default]
    #[serde(alias = "Detached")]
    #[baml(alias = "detached")]
    Detached,
    #[serde(alias = "ResumeCurrentTask")]
    #[baml(alias = "resume_current_task")]
    ResumeCurrentTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct CallbackCancelInput {
    #[serde(alias = "callback_id", skip_serializing_if = "Option::is_none")]
    #[baml(description = "Exact callback id to cancel.")]
    pub callback_id: Option<String>,
    #[serde(alias = "source_key", skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Source key for dedupe-key cancellation. Required when cancelling by dedupeKey."
    )]
    pub source_key: Option<String>,
    #[serde(alias = "dedupe_key", skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Optional dedupe key to cancel instead of a callback id. Requires sourceKey."
    )]
    pub dedupe_key: Option<String>,
}

impl baml_rt_tools::DescribeAction for CallbackCancelInput {
    fn describe(&self) -> String {
        match (&self.callback_id, &self.dedupe_key) {
            (Some(callback_id), _) => format!("cancelling callback '{callback_id}'"),
            (None, Some(dedupe_key)) => format!("cancelling deduped callback '{dedupe_key}'"),
            _ => "cancelling callback".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CallbackToolOutput {
    Scheduled(CallbackScheduledOutput),
    Cancelled(CallbackCancelledOutput),
}

impl TsType for CallbackToolOutput {
    fn ts_type_name() -> &'static str {
        "CallbackToolOutput"
    }

    fn ts_decl() -> Option<String> {
        tagged_union_ts_decl(
            Self::ts_type_name(),
            "outcome",
            &[
                ("scheduled", "CallbackScheduledOutput"),
                ("cancelled", "CallbackCancelledOutput"),
            ],
        )
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["CallbackScheduledOutput", "CallbackCancelledOutput"]
    }
}

impl JsonSchemaType for CallbackToolOutput {
    fn json_schema_inline() -> Value {
        tagged_union_schema(vec![
            tagged_union_variant_schema::<CallbackScheduledOutput>("outcome", "scheduled"),
            tagged_union_variant_schema::<CallbackCancelledOutput>("outcome", "cancelled"),
        ])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct CallbackScheduledOutput {
    pub callback_id: String,
    pub source_key: String,
    pub scheduled_for_unix_ms: u64,
    pub deduped: bool,
    /// Dispatch scope when the host minted a child context (detached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_task_id: Option<String>,
    /// Scheduling A2A scope used for delivery deferral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling_context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct CallbackCancelledOutput {
    pub cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
}

// --- system/discover_agents ---

/// Starts an agent-discovery session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverAgentsOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl baml_rt_tools::DescribeAction for DiscoverAgentsOpenInput {
    fn describe(&self) -> String {
        "discovering available agents".to_string()
    }
}

/// Requests one page of agents, optionally filtered by text, capability, or event subscription.
/// Send = one list request. Multiple Send/Read = multiple pages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverAgentsSendInput {
    /// Optional text filter over agent name, package, or description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Only return agents that declare every listed capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_capabilities: Option<Vec<String>>,
    /// Only return agents with a matching event subscription for any listed schema version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_schema_versions: Option<Vec<String>>,
    /// Only return agents with a matching event subscription for any listed source kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_source_kinds: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

impl baml_rt_tools::DescribeAction for DiscoverAgentsSendInput {
    fn describe(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("discovering agents matching '{q}'"),
            _ => "listing all available agents".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverAgentsNextOutput {
    pub agents: Vec<AgentCardDto>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct AgentCardDto {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_version: Option<u32>,
    pub agent_package: String,
    pub agent_instance_id: String,
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<AgentEventSubscriptionDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventSubscriptionDto {
    // Keep these fields in sync with the OpenAPI `AgentEventSubscriptionDto`.
    // This tool surface uses camelCase because system tool JSON is JS/TS-facing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_versions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_key_prefixes: Vec<String>,
}

// --- system/discover_tools ---

/// Starts a tool-discovery session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl baml_rt_tools::DescribeAction for DiscoverToolsOpenInput {
    fn describe(&self) -> String {
        "discovering available tools".to_string()
    }
}

/// Send = one search request. Multiple Send/Read cycles = multiple queries per session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsSendInput {
    /// Optional case-insensitive filter over tool name, bundle, or description.
    /// Omit or null to list all discoverable tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Optional maximum number of tools to return for this Send query.
    /// Use a small value when you want compact, token-efficient results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl baml_rt_tools::DescribeAction for DiscoverToolsSendInput {
    fn describe(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("discovering tools matching '{q}'"),
            _ => "listing all available tools".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsNextOutput {
    pub tools: Vec<ToolDiscoveryRecordDto>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiscoveryRecordDto {
    pub name: String,
    pub bundle: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_sources: Vec<String>,
}

// --- system/introspection + system/extrospection ---

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQueryOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl baml_rt_tools::DescribeAction for ProvenanceQueryOpenInput {
    fn describe(&self) -> String {
        "querying provenance records".to_string()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQuerySendInput {
    /// Optional runtime-general explicit read request envelope.
    /// When set, the tool resolves it and ignores resource/filter fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<SessionReadEnvelope>,
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baml_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_by: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

impl baml_rt_tools::DescribeAction for ProvenanceQuerySendInput {
    fn describe(&self) -> String {
        format!("querying provenance {resource}", resource = self.resource)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQueryNextOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_result: Option<SessionReadResultDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_budget: Option<ProvenanceRetrievalBudgetDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_exhausted: Option<bool>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceRetrievalBudgetDto {
    pub calls_used: u32,
    pub calls_cap: u32,
    pub bytes_used: u32,
    pub bytes_cap: u32,
    pub items_used: u32,
    pub items_cap: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceArchiveRecordDto {
    pub archive_ref: String,
    pub payloads: Vec<ProvenanceArchivePayloadDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceReadProjectionDto {
    Identity,
    Summary,
    Detail,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceArchiveSummaryDto {
    pub archive_ref: String,
    pub payload_count: u32,
    pub payload_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct ProvenancePayloadLlmCallDto {
    pub payload_ref: String,
    pub activity_ref: String,
    pub prompt_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct ProvenancePayloadLlmResultDto {
    pub payload_ref: String,
    pub activity_ref: String,
    pub result_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct ProvenancePayloadToolCallDto {
    pub payload_ref: String,
    pub activity_ref: String,
    pub tool_name: Option<String>,
    pub phase: Option<String>,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct ProvenancePayloadToolResultDto {
    pub payload_ref: String,
    pub activity_ref: String,
    pub result_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ProvenanceArchivePayloadDto {
    LlmCall(ProvenancePayloadLlmCallDto),
    LlmResult(ProvenancePayloadLlmResultDto),
    ToolCall(ProvenancePayloadToolCallDto),
    ToolResult(ProvenancePayloadToolResultDto),
}

impl TsType for ProvenanceArchivePayloadDto {
    fn ts_type_name() -> &'static str {
        "ProvenanceArchivePayloadDto"
    }

    fn ts_decl() -> Option<String> {
        tagged_union_ts_decl(
            Self::ts_type_name(),
            "source",
            &[
                ("llm_call", "ProvenancePayloadLlmCallDto"),
                ("llm_result", "ProvenancePayloadLlmResultDto"),
                ("tool_call", "ProvenancePayloadToolCallDto"),
                ("tool_result", "ProvenancePayloadToolResultDto"),
            ],
        )
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec![
            "ProvenancePayloadLlmCallDto",
            "ProvenancePayloadLlmResultDto",
            "ProvenancePayloadToolCallDto",
            "ProvenancePayloadToolResultDto",
        ]
    }
}

impl JsonSchemaType for ProvenanceArchivePayloadDto {
    fn json_schema_inline() -> Value {
        tagged_union_schema(vec![
            tagged_union_variant_schema::<ProvenancePayloadLlmCallDto>("source", "llm_call"),
            tagged_union_variant_schema::<ProvenancePayloadLlmResultDto>("source", "llm_result"),
            tagged_union_variant_schema::<ProvenancePayloadToolCallDto>("source", "tool_call"),
            tagged_union_variant_schema::<ProvenancePayloadToolResultDto>("source", "tool_result"),
        ])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResultDto {
    pub mode: SessionReadMode,
    pub ref_id: String,
    pub projection: ProvenanceReadProjectionDto,
    pub refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_summary: Option<ProvenanceArchiveSummaryDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_record: Option<ProvenanceArchiveRecordDto>,
}
