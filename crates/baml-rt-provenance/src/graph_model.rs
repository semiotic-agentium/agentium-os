// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use crate::{
    events::ProvEventData,
    vocabulary::{a2a, a2a_relations, a2a_roles, a2a_types, prov, prov_relations, semantic_labels},
};

/// Canonical node labels in the persisted provenance graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphNodeLabel {
    Intent,
    Plan,
    PlanStep,
    Message,
    MessageProcessing,
    LlmCall,
    ToolCall,
    LlmPrompt,
    ToolArgs,
    TaskExecution,
    Task,
    TaskState,
    Artifact,
    AgentBoot,
    AgentStop,
    AgentArchive,
    AgentRuntimeInstance,
    PromptRejected,
    FailureClassificationActivity,
    FailureClassification,
    /// An individual step within a tool session (Open/SendDone/Read).
    SessionStep,
}

impl GraphNodeLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "Intent",
            Self::Plan => "Plan",
            Self::PlanStep => "PlanStep",
            Self::Message => "Message",
            Self::MessageProcessing => "A2AMessageProcessing",
            Self::LlmCall => "LlmCall",
            Self::ToolCall => "ToolCall",
            Self::LlmPrompt => "LlmPrompt",
            Self::ToolArgs => "ToolArgs",
            Self::TaskExecution => "A2ATaskExecution",
            Self::Task => "A2ATask",
            Self::TaskState => "A2ATaskState",
            Self::Artifact => "Artifact",
            Self::AgentBoot => "AgentBoot",
            Self::AgentStop => "AgentStop",
            Self::AgentArchive => "AgentArchive",
            Self::AgentRuntimeInstance => "AgentRuntimeInstance",
            Self::PromptRejected => "PromptRejected",
            Self::FailureClassificationActivity => "FailureClassificationActivity",
            Self::FailureClassification => "FailureClassification",
            Self::SessionStep => "SessionStep",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Intent" => Some(Self::Intent),
            "Plan" => Some(Self::Plan),
            "PlanStep" => Some(Self::PlanStep),
            "Message" => Some(Self::Message),
            "A2AMessageProcessing" => Some(Self::MessageProcessing),
            "LlmCall" => Some(Self::LlmCall),
            "ToolCall" => Some(Self::ToolCall),
            "LlmPrompt" => Some(Self::LlmPrompt),
            "ToolArgs" => Some(Self::ToolArgs),
            "A2ATaskExecution" => Some(Self::TaskExecution),
            "A2ATask" => Some(Self::Task),
            "A2ATaskState" => Some(Self::TaskState),
            "Artifact" => Some(Self::Artifact),
            "AgentBoot" => Some(Self::AgentBoot),
            "AgentStop" => Some(Self::AgentStop),
            "AgentArchive" => Some(Self::AgentArchive),
            "AgentRuntimeInstance" => Some(Self::AgentRuntimeInstance),
            "PromptRejected" => Some(Self::PromptRejected),
            "FailureClassificationActivity" => Some(Self::FailureClassificationActivity),
            "FailureClassification" => Some(Self::FailureClassification),
            "SessionStep" => Some(Self::SessionStep),
            _ => None,
        }
    }
}

pub const EDGE_WAS_USED_BY: &str = semantic_labels::WAS_USED_BY;
pub const EDGE_WAS_CLASSIFIED_BY: &str = semantic_labels::WAS_CLASSIFIED_BY;
pub const EDGE_WAS_EXECUTED_BY: &str = semantic_labels::WAS_EXECUTED_BY;
pub const EDGE_WAS_INVOKED_BY: &str = semantic_labels::WAS_INVOKED_BY;
pub const EDGE_WAS_RECEIVED_BY: &str = semantic_labels::WAS_RECEIVED_BY;
pub const EDGE_WAS_EMITTED_BY: &str = semantic_labels::WAS_EMITTED_BY;
pub const EDGE_WAS_GENERATED_BY: &str = semantic_labels::WAS_GENERATED_BY;
pub const EDGE_WAS_CREATED_BY: &str = semantic_labels::WAS_CREATED_BY;
pub const EDGE_WAS_UPDATED_BY: &str = semantic_labels::WAS_UPDATED_BY;
pub const EDGE_WAS_TRANSITIONED_FROM: &str = semantic_labels::WAS_TRANSITIONED_FROM;
/// Head-pointer edge `Task -> TaskState` naming the head of the
/// `WAS_TRANSITIONED_FROM` chain. Re-pointed atomically by the normalizer
/// on every `TaskStatusChanged` event.
pub const EDGE_WAS_LAST_TRANSITIONED_TO: &str = semantic_labels::WAS_LAST_TRANSITIONED_TO;
/// Head-pointer edge `Task -> AgentRuntimeInstance` naming the
/// most-recent execution-owning agent. Re-pointed atomically by the
/// normalizer on every `TaskExecutionStarted` event.
pub const EDGE_WAS_LAST_EXECUTED_BY: &str = semantic_labels::WAS_LAST_EXECUTED_BY;
pub const EDGE_WAS_SPAWNED_BY: &str = semantic_labels::WAS_SPAWNED_BY;
/// Canonical edge label for `A2ATask → Message` linkage in either direction.
///
/// Sourced from [`a2a_relations::TASK_MESSAGE`] (`"A2A_TASK_MESSAGE"`). The
/// previously-defined `EDGE_TASK_TRIGGERED_BY_MESSAGE` and
/// `EDGE_TASK_EMITTED_MESSAGE` constants pointed at edge labels
/// (`TASK_TRIGGERED_BY_MESSAGE` / `TASK_EMITTED_MESSAGE`) that the normalizer
/// has never written. Both directions of the task↔message relation are
/// persisted as `A2A_TASK_MESSAGE` derived edges (see
/// [`crate::normalizer`] `A2aRelationType::TaskHasMessage`); the
/// `direction` attribute distinguishes received-vs-sent.
pub const EDGE_A2A_TASK_MESSAGE: &str = a2a_relations::TASK_MESSAGE;
pub const EDGE_WAS_SCHEDULED_FROM: &str = semantic_labels::WAS_SCHEDULED_FROM;
pub const EDGE_WAS_ASSOCIATED_WITH: &str = prov_relations::WAS_ASSOCIATED_WITH;

/// Event kinds mapped to graph relations/properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventGraphKind {
    IntentResolved,
    PlanGenerated,
    PlanStepStatusChanged,
    LlmCallStarted,
    LlmCallCompleted,
    PromptRejected,
    ToolCallStarted,
    ToolCallCompleted,
    AgentBooted,
    AgentStopped,
    TaskExists,
    TaskExecutionStarted,
    TaskExecutionEnded,
    TaskStatusChanged,
    TaskArtifactGenerated,
    MessageReceived,
    MessageSent,
    ToolSessionStep,
    ExternalToolLifecycle,
    CallbackDispatchContextsLinked,
    HostSourcePollRecorded,
    HostDispatchAccepted,
    HostDispatchRejected,
}

pub const ALL_EVENT_KINDS: [EventGraphKind; 23] = [
    EventGraphKind::IntentResolved,
    EventGraphKind::PlanGenerated,
    EventGraphKind::PlanStepStatusChanged,
    EventGraphKind::LlmCallStarted,
    EventGraphKind::LlmCallCompleted,
    EventGraphKind::PromptRejected,
    EventGraphKind::ToolCallStarted,
    EventGraphKind::ToolCallCompleted,
    EventGraphKind::AgentBooted,
    EventGraphKind::AgentStopped,
    EventGraphKind::TaskExists,
    EventGraphKind::TaskExecutionStarted,
    EventGraphKind::TaskExecutionEnded,
    EventGraphKind::TaskStatusChanged,
    EventGraphKind::TaskArtifactGenerated,
    EventGraphKind::MessageReceived,
    EventGraphKind::MessageSent,
    EventGraphKind::ToolSessionStep,
    EventGraphKind::ExternalToolLifecycle,
    EventGraphKind::CallbackDispatchContextsLinked,
    EventGraphKind::HostSourcePollRecorded,
    EventGraphKind::HostDispatchAccepted,
    EventGraphKind::HostDispatchRejected,
];

#[derive(Debug, Clone, Copy)]
pub struct EdgeContract {
    pub edge_label: &'static str,
    pub role_key: &'static str,
    pub role_value: &'static str,
    pub target_type_key: &'static str,
    pub target_type_value: &'static str,
}

pub const TOOL_CALL_ARGS_EDGE: EdgeContract = EdgeContract {
    edge_label: EDGE_WAS_USED_BY,
    role_key: prov::ROLE,
    role_value: a2a_roles::ARGS,
    target_type_key: prov::TYPE,
    target_type_value: a2a_types::TOOL_ARGS,
};

#[derive(Debug, Clone, Copy)]
pub struct EventGraphMapping {
    pub kind: EventGraphKind,
    pub primary_node: GraphNodeLabel,
    pub expected_edges: &'static [&'static str],
    pub required_properties: &'static [&'static str],
}

/// LLM call started.
///
/// **Edge endpoints** (LlmCall is `c`, prompt is `pr`, parent activity is
/// `p`):
/// - `(c:LlmCall) -[:WAS_USED_BY]-> (pr:LlmPrompt)` — LlmCall is from-end.
/// - `(p:A2AMessageProcessing|A2ATaskExecution) -[:WAS_INVOKED_BY]-> (c:LlmCall)`
///   — LlmCall is *to-end*; the parent activity owns the invocation.
///
/// **There is NO `LlmCall → AgentRuntimeInstance` edge.** The agent traversal
/// is two-hop via the parent activity. See [`ConversationGraphTraversal`].
const MAPPING_LLM_CALL_STARTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::LlmCallStarted,
    primary_node: GraphNodeLabel::LlmCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_INVOKED_BY],
    required_properties: &[a2a::CLIENT, a2a::MODEL, a2a::FUNCTION_NAME, a2a::AGENT_ID],
};

const MAPPING_INTENT_RESOLVED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::IntentResolved,
    primary_node: GraphNodeLabel::Intent,
    expected_edges: &[],
    required_properties: &[a2a::INTENT_ID, a2a::TASK_ID, a2a::CONTEXT_ID],
};

const MAPPING_PLAN_GENERATED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::PlanGenerated,
    primary_node: GraphNodeLabel::Plan,
    expected_edges: &[],
    required_properties: &[a2a::INTENT_ID, a2a::PLAN_ID, a2a::TASK_ID, a2a::CONTEXT_ID],
};

const MAPPING_PLAN_STEP_STATUS_CHANGED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::PlanStepStatusChanged,
    primary_node: GraphNodeLabel::PlanStep,
    expected_edges: &[],
    required_properties: &[a2a::INTENT_ID, a2a::PLAN_ID, a2a::STEP_ID, a2a::TASK_ID],
};

const MAPPING_LLM_CALL_COMPLETED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::LlmCallCompleted,
    primary_node: GraphNodeLabel::LlmCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_INVOKED_BY],
    required_properties: &[
        a2a::CLIENT,
        a2a::MODEL,
        a2a::FUNCTION_NAME,
        a2a::AGENT_ID,
        a2a::DURATION_MS,
        a2a::ACTIVITY_OUTCOME,
    ],
};

const MAPPING_PROMPT_REJECTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::PromptRejected,
    primary_node: GraphNodeLabel::PromptRejected,
    expected_edges: &[EDGE_WAS_USED_BY],
    required_properties: &[a2a::REASON],
};

/// Tool call started.
///
/// **Edge endpoints** (ToolCall is `c`, args is `args`, agent is `a`):
/// - `(c:ToolCall) -[:WAS_USED_BY]-> (args:ToolArgs)` — ToolCall is from-end.
/// - `(c:ToolCall) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)` — ToolCall
///   is from-end. **CONDITIONAL**: only emitted when the executing agent's id
///   resolves through `metadata.agent_id` or `NormalizeContext::task_agent_id`.
const MAPPING_TOOL_CALL_STARTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ToolCallStarted,
    primary_node: GraphNodeLabel::ToolCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::TOOL_NAME, a2a::AGENT_ID],
};

/// Tool call completed. Same edge contract as [`MAPPING_TOOL_CALL_STARTED`].
const MAPPING_TOOL_CALL_COMPLETED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ToolCallCompleted,
    primary_node: GraphNodeLabel::ToolCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[
        a2a::TOOL_NAME,
        a2a::AGENT_ID,
        a2a::DURATION_MS,
        a2a::ACTIVITY_OUTCOME,
    ],
};

const MAPPING_TOOL_SESSION_STEP: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ToolSessionStep,
    primary_node: GraphNodeLabel::SessionStep,
    expected_edges: &[],
    required_properties: &[a2a::TOOL_NAME],
};

const MAPPING_AGENT_BOOTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::AgentBooted,
    primary_node: GraphNodeLabel::AgentBoot,
    expected_edges: &[EDGE_WAS_SPAWNED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::AGENT_ID, a2a::AGENT_TYPE, a2a::AGENT_VERSION],
};

const MAPPING_AGENT_STOPPED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::AgentStopped,
    primary_node: GraphNodeLabel::AgentStop,
    expected_edges: &[EDGE_WAS_ASSOCIATED_WITH],
    required_properties: &[a2a::AGENT_ID],
};

const MAPPING_TASK_EXISTS: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::TaskExists,
    primary_node: GraphNodeLabel::Task,
    expected_edges: &[],
    required_properties: &[a2a::TASK_ID, a2a::CONTEXT_ID],
};

const MAPPING_TASK_EXECUTION_STARTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::TaskExecutionStarted,
    primary_node: GraphNodeLabel::TaskExecution,
    expected_edges: &[EDGE_WAS_CREATED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::TASK_ID, a2a::AGENT_ID, a2a::CONTEXT_ID],
};

const MAPPING_TASK_EXECUTION_ENDED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::TaskExecutionEnded,
    primary_node: GraphNodeLabel::TaskExecution,
    expected_edges: &[EDGE_WAS_CREATED_BY],
    required_properties: &[a2a::TASK_ID, a2a::CONTEXT_ID],
};

const MAPPING_TASK_STATUS_CHANGED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::TaskStatusChanged,
    primary_node: GraphNodeLabel::TaskState,
    expected_edges: &[EDGE_WAS_UPDATED_BY, EDGE_WAS_TRANSITIONED_FROM],
    required_properties: &[a2a::TASK_ID],
};

const MAPPING_TASK_ARTIFACT_GENERATED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::TaskArtifactGenerated,
    primary_node: GraphNodeLabel::Artifact,
    expected_edges: &[EDGE_WAS_GENERATED_BY],
    required_properties: &[a2a::TASK_ID],
};

/// Inbound message arrival.
///
/// **Edge endpoints** (Message is `m`, processing activity is `p`, agent is `a`,
/// task is `t`):
/// - `(p:A2AMessageProcessing) -[:WAS_RECEIVED_BY]-> (m:Message)` — the
///   processing activity records receipt of the inbound message. The
///   `Message` node is the *to-end* of this edge, not the from-end.
/// - `(t:A2ATask) -[:A2A_TASK_MESSAGE {direction: "inbound"}]-> (m:Message)` —
///   only when the inbound message is task-scoped.
///
/// **There is NO direct `Message → AgentRuntimeInstance` edge.** The agent
/// traversal is two-hop via the processing activity:
/// `(m:Message) <-[:WAS_RECEIVED_BY]- (p:A2AMessageProcessing) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)`.
/// See [`ConversationGraphTraversal`] for the canonical paths.
const MAPPING_MESSAGE_RECEIVED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::MessageReceived,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[EDGE_WAS_RECEIVED_BY, EDGE_A2A_TASK_MESSAGE],
    required_properties: &[a2a::MESSAGE_ID, a2a::ROLE, a2a::CONTENT, a2a::DIRECTION],
};

/// Outbound message emission.
///
/// **Edge endpoints**:
/// - `(m:Message) -[:WAS_EMITTED_BY]-> (p:A2AMessageProcessing)` — note the
///   `Message` is the *from-end* here, opposite to `MessageReceived`.
/// - `(t:A2ATask) -[:A2A_TASK_MESSAGE {direction: "outbound"}]-> (m:Message)` —
///   only when the outbound message is task-scoped.
///
/// Same two-hop traversal rule as `MessageReceived` for Message → Agent.
const MAPPING_MESSAGE_SENT: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::MessageSent,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[EDGE_WAS_EMITTED_BY, EDGE_A2A_TASK_MESSAGE],
    required_properties: &[a2a::MESSAGE_ID, a2a::ROLE, a2a::CONTENT, a2a::DIRECTION],
};

const MAPPING_EXTERNAL_TOOL_LIFECYCLE: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ExternalToolLifecycle,
    primary_node: GraphNodeLabel::ToolCall,
    expected_edges: &[],
    required_properties: &[a2a::TOOL_NAME],
};

const MAPPING_CALLBACK_DISPATCH_CONTEXTS_LINKED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::CallbackDispatchContextsLinked,
    primary_node: GraphNodeLabel::Task,
    expected_edges: &[EDGE_WAS_SCHEDULED_FROM],
    required_properties: &[a2a::TASK_ID, a2a::CONTEXT_ID],
};

const MAPPING_HOST_SOURCE_POLL_RECORDED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::HostSourcePollRecorded,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[],
    required_properties: &[
        a2a::CONTEXT_ID,
        a2a::ROLE,
        a2a::HOST_INGRESS_KIND,
        a2a::HOST_INGRESS_SOURCE_KIND,
        a2a::HOST_INGRESS_SOURCE_KEY,
    ],
};

const MAPPING_HOST_DISPATCH_ACCEPTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::HostDispatchAccepted,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[],
    required_properties: &[
        a2a::CONTEXT_ID,
        a2a::ROLE,
        a2a::HOST_INGRESS_KIND,
        a2a::HOST_INGRESS_TARGET_PACKAGE,
        a2a::HOST_INGRESS_TARGET_INSTANCE,
        a2a::HOST_INGRESS_ROUTING_KEY,
    ],
};

const MAPPING_HOST_DISPATCH_REJECTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::HostDispatchRejected,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[],
    required_properties: &[
        a2a::CONTEXT_ID,
        a2a::ROLE,
        a2a::HOST_INGRESS_KIND,
        a2a::HOST_INGRESS_TARGET_PACKAGE,
        a2a::HOST_INGRESS_TARGET_INSTANCE,
        a2a::HOST_INGRESS_ROUTING_KEY,
        a2a::REASON,
    ],
};

pub fn event_kind_from_data(data: &ProvEventData) -> EventGraphKind {
    match data {
        ProvEventData::IntentResolved { .. } => EventGraphKind::IntentResolved,
        ProvEventData::PlanGenerated { .. } => EventGraphKind::PlanGenerated,
        ProvEventData::PlanStepStatusChanged { .. } => EventGraphKind::PlanStepStatusChanged,
        ProvEventData::LlmCallStarted { .. } => EventGraphKind::LlmCallStarted,
        ProvEventData::LlmCallCompleted { .. } => EventGraphKind::LlmCallCompleted,
        ProvEventData::PromptRejected { .. } => EventGraphKind::PromptRejected,
        ProvEventData::ToolCallStarted { .. } => EventGraphKind::ToolCallStarted,
        ProvEventData::ToolCallCompleted { .. } => EventGraphKind::ToolCallCompleted,
        ProvEventData::AgentBooted { .. } => EventGraphKind::AgentBooted,
        ProvEventData::AgentStopped { .. } => EventGraphKind::AgentStopped,
        ProvEventData::TaskExists { .. } => EventGraphKind::TaskExists,
        ProvEventData::TaskExecutionStarted { .. } => EventGraphKind::TaskExecutionStarted,
        ProvEventData::TaskExecutionEnded { .. } => EventGraphKind::TaskExecutionEnded,
        ProvEventData::TaskStatusChanged { .. } => EventGraphKind::TaskStatusChanged,
        ProvEventData::TaskArtifactGenerated { .. } => EventGraphKind::TaskArtifactGenerated,
        ProvEventData::MessageReceived { .. } => EventGraphKind::MessageReceived,
        ProvEventData::MessageSent { .. } => EventGraphKind::MessageSent,
        ProvEventData::ToolSessionStep { .. } => EventGraphKind::ToolSessionStep,
        ProvEventData::ExternalToolLifecycle { .. } => EventGraphKind::ExternalToolLifecycle,
        ProvEventData::CallbackDispatchContextsLinked { .. } => {
            EventGraphKind::CallbackDispatchContextsLinked
        }
        ProvEventData::HostSourcePollRecorded { .. } => EventGraphKind::HostSourcePollRecorded,
        ProvEventData::HostDispatchAccepted { .. } => EventGraphKind::HostDispatchAccepted,
        ProvEventData::HostDispatchRejected { .. } => EventGraphKind::HostDispatchRejected,
    }
}

pub fn mapping_for_event_kind(kind: EventGraphKind) -> &'static EventGraphMapping {
    match kind {
        EventGraphKind::IntentResolved => &MAPPING_INTENT_RESOLVED,
        EventGraphKind::PlanGenerated => &MAPPING_PLAN_GENERATED,
        EventGraphKind::PlanStepStatusChanged => &MAPPING_PLAN_STEP_STATUS_CHANGED,
        EventGraphKind::LlmCallStarted => &MAPPING_LLM_CALL_STARTED,
        EventGraphKind::LlmCallCompleted => &MAPPING_LLM_CALL_COMPLETED,
        EventGraphKind::PromptRejected => &MAPPING_PROMPT_REJECTED,
        EventGraphKind::ToolCallStarted => &MAPPING_TOOL_CALL_STARTED,
        EventGraphKind::ToolCallCompleted => &MAPPING_TOOL_CALL_COMPLETED,
        EventGraphKind::AgentBooted => &MAPPING_AGENT_BOOTED,
        EventGraphKind::AgentStopped => &MAPPING_AGENT_STOPPED,
        EventGraphKind::TaskExists => &MAPPING_TASK_EXISTS,
        EventGraphKind::TaskExecutionStarted => &MAPPING_TASK_EXECUTION_STARTED,
        EventGraphKind::TaskExecutionEnded => &MAPPING_TASK_EXECUTION_ENDED,
        EventGraphKind::TaskStatusChanged => &MAPPING_TASK_STATUS_CHANGED,
        EventGraphKind::TaskArtifactGenerated => &MAPPING_TASK_ARTIFACT_GENERATED,
        EventGraphKind::MessageReceived => &MAPPING_MESSAGE_RECEIVED,
        EventGraphKind::MessageSent => &MAPPING_MESSAGE_SENT,
        EventGraphKind::ToolSessionStep => &MAPPING_TOOL_SESSION_STEP,
        EventGraphKind::ExternalToolLifecycle => &MAPPING_EXTERNAL_TOOL_LIFECYCLE,
        EventGraphKind::CallbackDispatchContextsLinked => {
            &MAPPING_CALLBACK_DISPATCH_CONTEXTS_LINKED
        }
        EventGraphKind::HostSourcePollRecorded => &MAPPING_HOST_SOURCE_POLL_RECORDED,
        EventGraphKind::HostDispatchAccepted => &MAPPING_HOST_DISPATCH_ACCEPTED,
        EventGraphKind::HostDispatchRejected => &MAPPING_HOST_DISPATCH_REJECTED,
    }
}

pub fn mapping_for_event_data(data: &ProvEventData) -> &'static EventGraphMapping {
    mapping_for_event_kind(event_kind_from_data(data))
}

/// Canonical read-query model for reconstructing conversation context from graph.
pub struct ConversationReadModel;

impl ConversationReadModel {
    pub const MESSAGE_COLUMN_COUNT: usize = 5;
    pub const TOOL_COLUMN_COUNT: usize = 7;

    /// Typed parameterised message query.
    pub fn message_query_storage_safe_params(context: &str) -> (String, serde_json::Value) {
        let query = "MATCH (m:Message) WHERE m.a2a_context_id = $context \
             RETURN m.a2a_activity_anchor, m.a2a_message_id, m.a2a_direction, m.a2a_role, m.a2a_content \
             ORDER BY m.a2a_activity_anchor";
        (query.to_string(), serde_json::json!({ "context": context }))
    }

    /// Typed parameterised tool query.
    pub fn tool_query_storage_safe_params(context: &str) -> (String, serde_json::Value) {
        let query = "MATCH (t:ToolCall) WHERE t.a2a_context_id = $context \
             MATCH (t)-[used:WAS_USED_BY]->(args:ToolArgs) \
             RETURN DISTINCT t.a2a_activity_anchor, t.a2a_tool_name, t.a2a_metadata, args.a2a_args, used.prov_role, args.prov_type, t.a2a_activity_outcome \
             ORDER BY t.a2a_activity_anchor";
        (query.to_string(), serde_json::json!({ "context": context }))
    }

    /// Session-step query: individual Open/SendDone/Read events within sessions.
    pub fn session_step_query_params(context: &str) -> (String, serde_json::Value) {
        let query = "MATCH (s:SessionStep) WHERE s.a2a_context_id = $context \
             RETURN s.a2a_activity_anchor, s.a2a_tool_name, s.op_kind, s.header, s.archive_ref, s.grep, s.offset, s.limit \
             ORDER BY s.a2a_activity_anchor";
        (query.to_string(), serde_json::json!({ "context": context }))
    }
}

/// Doc-only catalogue of canonical multi-hop graph traversals.
///
/// Read paths must traverse these edges rather than filter by denormalised
/// properties (`a2a_context_id`, `a2a_agent_id`, `a2a_task_id`, …). The Phase
/// 0.5 typed-metamodel surface (`metamodel::query::GraphQuery`) encodes each
/// of these paths as a named constructor; this struct documents the canonical
/// shape for human readers and serves as a doctrinal anchor when extending the
/// typed surface.
///
/// **Why two-hop matters:** there is NO direct `Message → AgentRuntimeInstance`,
/// `LlmCall → AgentRuntimeInstance`, or `ToolCall → AgentArchive` edge in the
/// persisted graph. Those traversals route through intermediate activities
/// (`A2AMessageProcessing`, `A2ATaskExecution`) and identity nodes
/// (`AgentBoot`).
pub struct ConversationGraphTraversal;

impl ConversationGraphTraversal {
    /// Message → owning agent (two hops via the processing activity).
    ///
    /// `(m:Message) <-[:WAS_RECEIVED_BY|:WAS_EMITTED_BY]- (p:A2AMessageProcessing) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)`
    ///
    /// `WAS_RECEIVED_BY` direction: `p → m` (message was received by the
    /// processing activity). `WAS_EMITTED_BY` direction: `m → p` (message was
    /// emitted *by* the processing activity, so the message is the from-end).
    /// Reads should follow either edge depending on the direction filter.
    pub const MESSAGE_TO_AGENT: &'static str = "(m:Message) <-[:WAS_RECEIVED_BY]- (p:A2AMessageProcessing) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance) \
         UNION \
         (m:Message) -[:WAS_EMITTED_BY]-> (p:A2AMessageProcessing) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)";

    /// Agent runtime instance → archive (two hops via boot activity).
    ///
    /// `(a:AgentRuntimeInstance) -[:WAS_SPAWNED_BY]-> (b:AgentBoot) -[:WAS_BOOTSTRAPPED_BY]-> (arc:AgentArchive)`
    ///
    /// The `agent_package` and `archive_path` attributes live on the archive,
    /// not on the runtime instance. Reads needing `agent_package` must take
    /// this two-hop traversal.
    pub const AGENT_TO_ARCHIVE: &'static str = "(a:AgentRuntimeInstance) -[:WAS_SPAWNED_BY]-> (b:AgentBoot) -[:WAS_BOOTSTRAPPED_BY]-> (arc:AgentArchive)";

    /// LlmCall → invoking agent (two hops via parent activity).
    ///
    /// `(c:LlmCall) <-[:WAS_INVOKED_BY]- (p:A2AMessageProcessing|A2ATaskExecution) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)`
    pub const LLM_CALL_TO_AGENT: &'static str = "(c:LlmCall) <-[:WAS_INVOKED_BY]- (p:A2AMessageProcessing|A2ATaskExecution) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)";

    /// Any scoped node → its owning context (one hop via SCOPED_TO).
    ///
    /// `(n:Subject) -[:SCOPED_TO]-> (ctx:Context)`
    ///
    /// Emitted by `surreal_write_batch` for every entity / activity / agent
    /// in a normalized fragment when the event carries a context, except for
    /// labels in `vocabulary::context_scope::SCOPE_EXEMPT_LABELS`
    /// (`AgentBoot`, `AgentArchive`, `AgentStop`).
    pub const SCOPED_TO_CONTEXT: &'static str = "(n) -[:SCOPED_TO]-> (ctx:Context)";

    /// Task → message linkage (single hop, single semantic edge label).
    ///
    /// `(t:A2ATask) -[:A2A_TASK_MESSAGE {direction: 'inbound'|'outbound'}]-> (m:Message)`
    ///
    /// The `direction` edge attribute distinguishes inbound (was triggered by
    /// the message) and outbound (emitted the message). The previously
    /// documented `TASK_TRIGGERED_BY_MESSAGE` / `TASK_EMITTED_MESSAGE` edge
    /// labels do not exist on disk.
    pub const TASK_TO_MESSAGE: &'static str =
        "(t:A2ATask) -[:A2A_TASK_MESSAGE {direction}]-> (m:Message)";
}
