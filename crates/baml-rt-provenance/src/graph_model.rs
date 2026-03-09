use crate::{
    events::ProvEventData,
    vocabulary::{a2a, a2a_roles, a2a_types, prov, semantic_labels},
};

/// Canonical node labels in the persisted provenance graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphNodeLabel {
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
    AgentArchive,
    AgentRuntimeInstance,
    PromptRejected,
    FailureClassificationActivity,
    FailureClassification,
}

impl GraphNodeLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
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
            Self::AgentArchive => "AgentArchive",
            Self::AgentRuntimeInstance => "AgentRuntimeInstance",
            Self::PromptRejected => "PromptRejected",
            Self::FailureClassificationActivity => "FailureClassificationActivity",
            Self::FailureClassification => "FailureClassification",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
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
            "AgentArchive" => Some(Self::AgentArchive),
            "AgentRuntimeInstance" => Some(Self::AgentRuntimeInstance),
            "PromptRejected" => Some(Self::PromptRejected),
            "FailureClassificationActivity" => Some(Self::FailureClassificationActivity),
            "FailureClassification" => Some(Self::FailureClassification),
            _ => None,
        }
    }
}

pub const EDGE_WAS_USED_BY: &str = semantic_labels::WAS_USED_BY;
pub const EDGE_WAS_EXECUTED_BY: &str = semantic_labels::WAS_EXECUTED_BY;
pub const EDGE_WAS_INVOKED_BY: &str = semantic_labels::WAS_INVOKED_BY;
pub const EDGE_WAS_RECEIVED_BY: &str = semantic_labels::WAS_RECEIVED_BY;
pub const EDGE_WAS_EMITTED_BY: &str = semantic_labels::WAS_EMITTED_BY;
pub const EDGE_WAS_GENERATED_BY: &str = semantic_labels::WAS_GENERATED_BY;
pub const EDGE_WAS_CREATED_BY: &str = semantic_labels::WAS_CREATED_BY;
pub const EDGE_WAS_UPDATED_BY: &str = semantic_labels::WAS_UPDATED_BY;
pub const EDGE_WAS_TRANSITIONED_FROM: &str = semantic_labels::WAS_TRANSITIONED_FROM;
pub const EDGE_WAS_SPAWNED_BY: &str = semantic_labels::WAS_SPAWNED_BY;
pub const EDGE_TASK_TRIGGERED_BY_MESSAGE: &str = semantic_labels::TASK_TRIGGERED_BY_MESSAGE;
pub const EDGE_TASK_EMITTED_MESSAGE: &str = semantic_labels::TASK_EMITTED_MESSAGE;

/// Event kinds mapped to graph relations/properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventGraphKind {
    LlmCallStarted,
    LlmCallCompleted,
    PromptRejected,
    ToolCallStarted,
    ToolCallCompleted,
    AgentBooted,
    TaskExists,
    TaskExecutionStarted,
    TaskExecutionEnded,
    TaskStatusChanged,
    TaskArtifactGenerated,
    MessageReceived,
    MessageSent,
}

pub const ALL_EVENT_KINDS: [EventGraphKind; 13] = [
    EventGraphKind::LlmCallStarted,
    EventGraphKind::LlmCallCompleted,
    EventGraphKind::PromptRejected,
    EventGraphKind::ToolCallStarted,
    EventGraphKind::ToolCallCompleted,
    EventGraphKind::AgentBooted,
    EventGraphKind::TaskExists,
    EventGraphKind::TaskExecutionStarted,
    EventGraphKind::TaskExecutionEnded,
    EventGraphKind::TaskStatusChanged,
    EventGraphKind::TaskArtifactGenerated,
    EventGraphKind::MessageReceived,
    EventGraphKind::MessageSent,
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

const MAPPING_LLM_CALL_STARTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::LlmCallStarted,
    primary_node: GraphNodeLabel::LlmCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_INVOKED_BY],
    required_properties: &[a2a::CLIENT, a2a::MODEL, a2a::FUNCTION_NAME, a2a::AGENT_ID],
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

const MAPPING_TOOL_CALL_STARTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ToolCallStarted,
    primary_node: GraphNodeLabel::ToolCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::TOOL_NAME, a2a::AGENT_ID],
};

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

const MAPPING_AGENT_BOOTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::AgentBooted,
    primary_node: GraphNodeLabel::AgentBoot,
    expected_edges: &[EDGE_WAS_SPAWNED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::AGENT_ID, a2a::AGENT_TYPE, a2a::AGENT_VERSION],
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

const MAPPING_MESSAGE_RECEIVED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::MessageReceived,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[EDGE_WAS_RECEIVED_BY, EDGE_TASK_TRIGGERED_BY_MESSAGE],
    required_properties: &[a2a::MESSAGE_ID, a2a::ROLE, a2a::CONTENT, a2a::DIRECTION],
};

const MAPPING_MESSAGE_SENT: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::MessageSent,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[EDGE_WAS_EMITTED_BY, EDGE_TASK_EMITTED_MESSAGE],
    required_properties: &[a2a::MESSAGE_ID, a2a::ROLE, a2a::CONTENT, a2a::DIRECTION],
};

pub fn event_kind_from_data(data: &ProvEventData) -> EventGraphKind {
    match data {
        ProvEventData::LlmCallStarted { .. } => EventGraphKind::LlmCallStarted,
        ProvEventData::LlmCallCompleted { .. } => EventGraphKind::LlmCallCompleted,
        ProvEventData::PromptRejected { .. } => EventGraphKind::PromptRejected,
        ProvEventData::ToolCallStarted { .. } => EventGraphKind::ToolCallStarted,
        ProvEventData::ToolCallCompleted { .. } => EventGraphKind::ToolCallCompleted,
        ProvEventData::AgentBooted { .. } => EventGraphKind::AgentBooted,
        ProvEventData::TaskExists { .. } => EventGraphKind::TaskExists,
        ProvEventData::TaskExecutionStarted { .. } => EventGraphKind::TaskExecutionStarted,
        ProvEventData::TaskExecutionEnded { .. } => EventGraphKind::TaskExecutionEnded,
        ProvEventData::TaskStatusChanged { .. } => EventGraphKind::TaskStatusChanged,
        ProvEventData::TaskArtifactGenerated { .. } => EventGraphKind::TaskArtifactGenerated,
        ProvEventData::MessageReceived { .. } => EventGraphKind::MessageReceived,
        ProvEventData::MessageSent { .. } => EventGraphKind::MessageSent,
    }
}

pub fn mapping_for_event_kind(kind: EventGraphKind) -> &'static EventGraphMapping {
    match kind {
        EventGraphKind::LlmCallStarted => &MAPPING_LLM_CALL_STARTED,
        EventGraphKind::LlmCallCompleted => &MAPPING_LLM_CALL_COMPLETED,
        EventGraphKind::PromptRejected => &MAPPING_PROMPT_REJECTED,
        EventGraphKind::ToolCallStarted => &MAPPING_TOOL_CALL_STARTED,
        EventGraphKind::ToolCallCompleted => &MAPPING_TOOL_CALL_COMPLETED,
        EventGraphKind::AgentBooted => &MAPPING_AGENT_BOOTED,
        EventGraphKind::TaskExists => &MAPPING_TASK_EXISTS,
        EventGraphKind::TaskExecutionStarted => &MAPPING_TASK_EXECUTION_STARTED,
        EventGraphKind::TaskExecutionEnded => &MAPPING_TASK_EXECUTION_ENDED,
        EventGraphKind::TaskStatusChanged => &MAPPING_TASK_STATUS_CHANGED,
        EventGraphKind::TaskArtifactGenerated => &MAPPING_TASK_ARTIFACT_GENERATED,
        EventGraphKind::MessageReceived => &MAPPING_MESSAGE_RECEIVED,
        EventGraphKind::MessageSent => &MAPPING_MESSAGE_SENT,
    }
}

pub fn mapping_for_event_data(data: &ProvEventData) -> &'static EventGraphMapping {
    mapping_for_event_kind(event_kind_from_data(data))
}

/// Canonical read-query model for reconstructing conversation context from graph.
pub struct ConversationReadModel;

impl ConversationReadModel {
    pub const MESSAGE_COLUMN_COUNT: usize = 6;
    pub const TOOL_COLUMN_COUNT: usize = 8;

    /// Typed parameterised message query for cypher_builder().params().run().
    pub fn message_query_storage_safe_params(context: &str) -> (String, serde_json::Value) {
        let query = "MATCH (m:Message) WHERE m.a2a_context_id = $context \
             RETURN m.a2a_event_id, m.a2a_message_id, m.a2a_direction, m.a2a_role, m.a2a_content, m.a2a_agent_id \
             ORDER BY m.a2a_event_id";
        (query.to_string(), serde_json::json!({ "context": context }))
    }

    /// Typed parameterised tool query for cypher_builder().params().run().
    pub fn tool_query_storage_safe_params(context: &str) -> (String, serde_json::Value) {
        let query = "MATCH (t:ToolCall) WHERE t.a2a_context_id = $context \
             MATCH (t)-[used:WAS_USED_BY]->(args:ToolArgs) \
             RETURN DISTINCT t.a2a_event_id, t.a2a_tool_name, t.a2a_metadata, args.a2a_args, used.prov_role, args.prov_type, t.a2a_activity_outcome, t.a2a_agent_id \
             ORDER BY t.a2a_event_id";
        (query.to_string(), serde_json::json!({ "context": context }))
    }
}
