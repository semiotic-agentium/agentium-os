//! Structured request and response types for system tools.

use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use baml_rt_tools::tools::{HistoryContextV1, SessionReadEnvelope, SessionReadMode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct InternalA2aTarget {
    pub agent_package: String,
    pub agent_instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct InternalA2aOpenInput {
    pub target: InternalA2aTarget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
/// Message sent to a remote agent.
/// Provide at least one part with text content.
pub struct InternalA2aSendInput {
    /// Message parts. Provide at least one with a non-empty text field.
    pub parts: Vec<ConversationPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<ConversationPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InternalA2aCompletion {
    #[default]
    Done,
    InputRequired,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct InternalA2aNextOutput {
    pub chunks: Vec<ConversationChunk>,
    /// Present when the delegated agent paused and needs more input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<InternalA2aCompletion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

// --- system/discover_agents ---

/// Starts an agent-discovery session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverAgentsNextOutput {
    pub agents: Vec<AgentCardDto>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AgentCardDto {
    pub name: String,
    pub version: String,
    pub agent_package: String,
    pub agent_instance_id: String,
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<AgentEventSubscriptionDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsNextOutput {
    pub tools: Vec<ToolDiscoveryRecordDto>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_context: Option<HistoryContextV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQuerySendInput {
    /// Optional runtime-general explicit read request envelope.
    /// When set, the tool resolves it and ignores resource/filter fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<SessionReadEnvelope>,
    pub resource: String,
    #[ts(type = "string | null")]
    #[schemars(with = "Option<String>")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[ts(type = "string | null")]
    #[schemars(with = "Option<String>")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[ts(type = "string | null")]
    #[schemars(with = "Option<String>")]
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
        format!("querying provenance {}", self.resource)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceRetrievalBudgetDto {
    pub calls_used: u32,
    pub calls_cap: u32,
    pub bytes_used: u32,
    pub bytes_cap: u32,
    pub items_used: u32,
    pub items_cap: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceArchiveRecordDto {
    pub archive_ref: String,
    pub payloads: Vec<ProvenanceArchivePayloadDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceReadProjectionDto {
    Identity,
    Summary,
    Detail,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceArchiveSummaryDto {
    pub archive_ref: String,
    pub payload_count: u32,
    pub payload_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ProvenanceArchivePayloadDto {
    LlmCall {
        payload_ref: String,
        activity_ref: String,
        prompt_json: String,
    },
    LlmResult {
        payload_ref: String,
        activity_ref: String,
        result_json: String,
    },
    ToolCall {
        payload_ref: String,
        activity_ref: String,
        tool_name: Option<String>,
        phase: Option<String>,
        args_json: String,
    },
    ToolResult {
        payload_ref: String,
        activity_ref: String,
        result_json: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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
