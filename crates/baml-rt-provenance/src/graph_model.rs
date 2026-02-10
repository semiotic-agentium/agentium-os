use crate::events::ProvEventData;
use crate::vocabulary::{a2a, a2a_roles, a2a_types, prov, semantic_labels};

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

/// Event kinds mapped to graph relations/properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventGraphKind {
    LlmCallStarted,
    LlmCallCompleted,
    ToolCallStarted,
    ToolCallCompleted,
    AgentBooted,
    TaskCreated,
    TaskStatusChanged,
    TaskArtifactGenerated,
    MessageReceived,
    MessageSent,
}

pub const ALL_EVENT_KINDS: [EventGraphKind; 10] = [
    EventGraphKind::LlmCallStarted,
    EventGraphKind::LlmCallCompleted,
    EventGraphKind::ToolCallStarted,
    EventGraphKind::ToolCallCompleted,
    EventGraphKind::AgentBooted,
    EventGraphKind::TaskCreated,
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
    required_properties: &[a2a::CLIENT, a2a::MODEL, a2a::FUNCTION_NAME, a2a::METADATA],
};

const MAPPING_LLM_CALL_COMPLETED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::LlmCallCompleted,
    primary_node: GraphNodeLabel::LlmCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_INVOKED_BY],
    required_properties: &[
        a2a::CLIENT,
        a2a::MODEL,
        a2a::FUNCTION_NAME,
        a2a::METADATA,
        a2a::DURATION_MS,
        a2a::SUCCESS,
    ],
};

const MAPPING_TOOL_CALL_STARTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ToolCallStarted,
    primary_node: GraphNodeLabel::ToolCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::TOOL_NAME, a2a::METADATA],
};

const MAPPING_TOOL_CALL_COMPLETED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::ToolCallCompleted,
    primary_node: GraphNodeLabel::ToolCall,
    expected_edges: &[EDGE_WAS_USED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[
        a2a::TOOL_NAME,
        a2a::METADATA,
        a2a::DURATION_MS,
        a2a::SUCCESS,
    ],
};

const MAPPING_AGENT_BOOTED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::AgentBooted,
    primary_node: GraphNodeLabel::AgentBoot,
    expected_edges: &[EDGE_WAS_SPAWNED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::AGENT_ID, a2a::AGENT_TYPE, a2a::AGENT_VERSION],
};

const MAPPING_TASK_CREATED: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::TaskCreated,
    primary_node: GraphNodeLabel::Task,
    expected_edges: &[EDGE_WAS_CREATED_BY, EDGE_WAS_EXECUTED_BY],
    required_properties: &[a2a::TASK_ID, a2a::AGENT_ID],
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
    expected_edges: &[EDGE_WAS_RECEIVED_BY, EDGE_WAS_SPAWNED_BY],
    required_properties: &[a2a::MESSAGE_ID, a2a::ROLE, a2a::CONTENT, a2a::DIRECTION],
};

const MAPPING_MESSAGE_SENT: EventGraphMapping = EventGraphMapping {
    kind: EventGraphKind::MessageSent,
    primary_node: GraphNodeLabel::Message,
    expected_edges: &[EDGE_WAS_EMITTED_BY, EDGE_WAS_SPAWNED_BY],
    required_properties: &[a2a::MESSAGE_ID, a2a::ROLE, a2a::CONTENT, a2a::DIRECTION],
};

pub fn event_kind_from_data(data: &ProvEventData) -> EventGraphKind {
    match data {
        ProvEventData::LlmCallStarted { .. } => EventGraphKind::LlmCallStarted,
        ProvEventData::LlmCallCompleted { .. } => EventGraphKind::LlmCallCompleted,
        ProvEventData::ToolCallStarted { .. } => EventGraphKind::ToolCallStarted,
        ProvEventData::ToolCallCompleted { .. } => EventGraphKind::ToolCallCompleted,
        ProvEventData::AgentBooted { .. } => EventGraphKind::AgentBooted,
        ProvEventData::TaskCreated { .. } => EventGraphKind::TaskCreated,
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
        EventGraphKind::ToolCallStarted => &MAPPING_TOOL_CALL_STARTED,
        EventGraphKind::ToolCallCompleted => &MAPPING_TOOL_CALL_COMPLETED,
        EventGraphKind::AgentBooted => &MAPPING_AGENT_BOOTED,
        EventGraphKind::TaskCreated => &MAPPING_TASK_CREATED,
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
    pub const MESSAGE_COLUMN_COUNT: usize = 5;
    pub const TOOL_COLUMN_COUNT: usize = 6;

    pub fn message_query(context: &str) -> String {
        let message_label = GraphNodeLabel::Message.as_str();
        format!(
            "MATCH (m:{message_label}) \
             WHERE m.`a2a:context_id` = \"{context}\" \
             RETURN m.`a2a:event_id`, m.`a2a:message_id`, m.`a2a:direction`, m.`a2a:role`, m.`a2a:content` \
             ORDER BY m.`a2a:event_id`"
        )
    }

    pub fn tool_query(context: &str) -> String {
        let message_processing_label = GraphNodeLabel::MessageProcessing.as_str();
        let tool_args_role = TOOL_CALL_ARGS_EDGE.role_key;
        let tool_args_type = TOOL_CALL_ARGS_EDGE.target_type_key;
        format!(
            "MATCH (mp:{message_processing_label})-[:{EDGE_WAS_EXECUTED_BY}]->(t) \
             WHERE mp.`a2a:context_id` = \"{context}\" AND t.name STARTS WITH \"tool_call:\" \
             MATCH (t)-[used]->(args) \
             RETURN t.`a2a:event_id`, t.`a2a:tool_name`, toString(t.`a2a:metadata`), toString(args.`a2a:args`), used.`{tool_args_role}`, args.`{tool_args_type}` \
             ORDER BY t.`a2a:event_id`"
        )
    }
}
