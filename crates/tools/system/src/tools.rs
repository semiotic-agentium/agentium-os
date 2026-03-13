//! Structured request and response types for system tools.

use baml_rt_core::ids::{AgentId, ContextId, TaskId};
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
pub struct InternalA2aSendInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<ConversationPart>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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

/// Why another agent stopped producing output.
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

/// Requests one page of agents, optionally filtered by query or capability.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverAgentsNextOutput {
    pub agents: Vec<AgentCardDto>,
    pub done: bool,
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
}

// --- system/workflow_routing ---

/// Starts a workflow-routing session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoutingOpenInput {
    /// Optional short reason for looking up routing policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq, Hash)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRoutingDecisionKind {
    CreatePmWork,
    ExecuteExistingWork,
    CancelOrCloseWork,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq, Hash)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRoutingSourceKind {
    Slack,
    Clickup,
    GithubIssues,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoutingSendInput {
    pub decision_kind: WorkflowRoutingDecisionKind,
    pub source_kind: WorkflowRoutingSourceKind,
    pub source_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoutingNextOutput {
    pub required_capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_agent_package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
    pub done: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoutingRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_kinds: Vec<WorkflowRoutingDecisionKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_kinds: Vec<WorkflowRoutingSourceKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_key_prefixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_agent_package: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoutingConfig {
    #[serde(default = "default_workflow_routing_rules")]
    pub routes: Vec<WorkflowRoutingRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_route: Option<WorkflowRoutingRule>,
}

fn default_workflow_routing_rules() -> Vec<WorkflowRoutingRule> {
    vec![
        WorkflowRoutingRule {
            name: Some("slack-create-pm-work".to_string()),
            decision_kinds: vec![WorkflowRoutingDecisionKind::CreatePmWork],
            source_kinds: vec![WorkflowRoutingSourceKind::Slack],
            required_capabilities: vec!["clickup:create-task".to_string()],
            preferred_agent_package: Some("clickup-agent".to_string()),
            ..WorkflowRoutingRule::default()
        },
        WorkflowRoutingRule {
            name: Some("clickup-execute-existing-work".to_string()),
            decision_kinds: vec![WorkflowRoutingDecisionKind::ExecuteExistingWork],
            source_kinds: vec![WorkflowRoutingSourceKind::Clickup],
            required_capabilities: vec!["coordination:routing".to_string()],
            preferred_agent_package: Some("coordinator-agent".to_string()),
            ..WorkflowRoutingRule::default()
        },
        WorkflowRoutingRule {
            name: Some("clickup-cancel-or-close-work".to_string()),
            decision_kinds: vec![WorkflowRoutingDecisionKind::CancelOrCloseWork],
            source_kinds: vec![WorkflowRoutingSourceKind::Clickup],
            required_capabilities: vec!["coordination:routing".to_string()],
            preferred_agent_package: Some("coordinator-agent".to_string()),
            ..WorkflowRoutingRule::default()
        },
        WorkflowRoutingRule {
            name: Some("github-execute-existing-work".to_string()),
            decision_kinds: vec![WorkflowRoutingDecisionKind::ExecuteExistingWork],
            source_kinds: vec![WorkflowRoutingSourceKind::GithubIssues],
            required_capabilities: vec!["coordination:routing".to_string()],
            preferred_agent_package: Some("coordinator-agent".to_string()),
            ..WorkflowRoutingRule::default()
        },
        WorkflowRoutingRule {
            name: Some("github-cancel-or-close-work".to_string()),
            decision_kinds: vec![WorkflowRoutingDecisionKind::CancelOrCloseWork],
            source_kinds: vec![WorkflowRoutingSourceKind::GithubIssues],
            required_capabilities: vec!["coordination:routing".to_string()],
            preferred_agent_package: Some("coordinator-agent".to_string()),
            ..WorkflowRoutingRule::default()
        },
    ]
}

impl Default for WorkflowRoutingConfig {
    fn default() -> Self {
        Self {
            routes: default_workflow_routing_rules(),
            default_route: None,
        }
    }
}

// --- system/discover_tools ---

/// Starts a tool-discovery session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsOpenInput {
    /// Optional short justification for choosing to use discover_tools (e.g. "user asked what tools are available").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Requests one page of tools, optionally filtered by query.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsSendInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverToolsNextOutput {
    pub tools: Vec<ToolDiscoveryRecordDto>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiscoveryRecordDto {
    pub name: String,
    pub bundle: String,
    pub description: String,
    pub tags: Vec<String>,
}

// --- system/introspection + system/extrospection ---

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQueryOpenInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQuerySendInput {
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceQueryNextOutput {
    pub payload_json: String,
    pub done: bool,
}
