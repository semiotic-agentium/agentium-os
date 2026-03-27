//! Structured request and response types for system tools.

use baml_derive::BamlType;
use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use baml_rt_tools::tools::{HistoryContextV1, SessionReadEnvelope, SessionReadMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[baml(union)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CallbackToolInput {
    Schedule(CallbackScheduleInput),
    Cancel(CallbackCancelInput),
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
    #[baml(description = "Delay before the callback event is emitted, in milliseconds.")]
    pub after_ms: u64,
    #[baml(
        description = "Stable event source key for subscription matching, for example `workflow-intake:follow-up`."
    )]
    pub source_key: String,
    #[baml(description = "Opaque JSON payload delivered back through onDispatch.")]
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Optional idempotency key scoped to the sourceKey. A pending callback with the same sourceKey + dedupeKey is reused instead of creating another row."
    )]
    pub dedupe_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Optional continuation policy. Omit it for a detached callback. Use `resume_current_task` to re-enter the current task later in the same context."
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackContinuationMode {
    #[default]
    Detached,
    ResumeCurrentTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct CallbackCancelInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(description = "Exact callback id to cancel.")]
    pub callback_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[baml(
        description = "Source key for dedupe-key cancellation. Required when cancelling by dedupeKey."
    )]
    pub source_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[baml(union)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CallbackToolOutput {
    Scheduled(CallbackScheduledOutput),
    Cancelled(CallbackCancelledOutput),
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct CallbackScheduledOutput {
    pub callback_id: String,
    pub source_key: String,
    pub scheduled_for_unix_ms: u64,
    pub deduped: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[baml(union)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ProvenanceArchivePayloadDto {
    LlmCall(ProvenancePayloadLlmCallDto),
    LlmResult(ProvenancePayloadLlmResultDto),
    ToolCall(ProvenancePayloadToolCallDto),
    ToolResult(ProvenancePayloadToolResultDto),
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
