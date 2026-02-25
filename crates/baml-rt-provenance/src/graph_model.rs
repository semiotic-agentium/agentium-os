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
    pub const TOOL_COLUMN_COUNT: usize = 7;

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
        let tool_call_label = GraphNodeLabel::ToolCall.as_str();
        let tool_args_label = GraphNodeLabel::ToolArgs.as_str();
        let tool_args_edge = TOOL_CALL_ARGS_EDGE.edge_label;
        let tool_args_role = TOOL_CALL_ARGS_EDGE.role_key;
        let tool_args_type = TOOL_CALL_ARGS_EDGE.target_type_key;
        // Match ToolCall nodes directly by context_id. Every ToolCall activity
        // carries a2a:context_id from base_attrs, so we don't need to traverse
        // the parent (A2AMessageProcessing or A2ATaskExecution) at all. This
        // works for both message-scoped and task-scoped tool calls.
        //
        // The second MATCH is constrained to WAS_USED_BY edges targeting
        // ToolArgs nodes to avoid picking up other outgoing edges (e.g.
        // WAS_EXECUTED_BY to AgentRuntimeInstance).
        format!(
            "MATCH (t:{tool_call_label}) \
             WHERE t.`a2a:context_id` = \"{context}\" \
             MATCH (t)-[used:{tool_args_edge}]->(args:{tool_args_label}) \
             RETURN DISTINCT t.`a2a:event_id`, t.`a2a:tool_name`, t.`a2a:metadata`, args.`a2a:args`, used.`{tool_args_role}`, args.`{tool_args_type}`, t.`a2a:success` \
             ORDER BY t.`a2a:event_id`"
        )
    }

    /// Message query using storage-safe property names (a2a_context_id etc.; colons replaced by underscores).
    /// Use for GraphQLite literal MERGE path ([KeyStyle::StorageSafeUnderscore]).
    pub fn message_query_storage_safe(context: &str) -> String {
        let message_label = GraphNodeLabel::Message.as_str();
        format!(
            "MATCH (m:{message_label}) \
             WHERE m.a2a_context_id = \"{context}\" \
             RETURN m.a2a_event_id, m.a2a_message_id, m.a2a_direction, m.a2a_role, m.a2a_content \
             ORDER BY m.a2a_event_id"
        )
    }

    /// Typed parameterised message query for cypher_builder().params().run().
    /// Property names use underscore form to match [crate::cypher_build::KeyStyle::StorageSafeUnderscore] (literal MERGE).
    pub fn message_query_storage_safe_params(context: &str) -> (&'static str, serde_json::Value) {
        const QUERY: &str = "MATCH (m:Message) WHERE m.a2a_context_id = $context \
             RETURN m.a2a_event_id, m.a2a_message_id, m.a2a_direction, m.a2a_role, m.a2a_content \
             ORDER BY m.a2a_event_id";
        let params = serde_json::json!({ "context": context });
        (QUERY, params)
    }

    /// Tool query using storage-safe property names (underscore form). Use for GraphQLite.
    pub fn tool_query_storage_safe(context: &str) -> String {
        let tool_call_label = GraphNodeLabel::ToolCall.as_str();
        let tool_args_label = GraphNodeLabel::ToolArgs.as_str();
        let tool_args_edge = TOOL_CALL_ARGS_EDGE.edge_label;
        format!(
            "MATCH (t:{tool_call_label}) \
             WHERE t.a2a_context_id = \"{context}\" \
             MATCH (t)-[used:{tool_args_edge}]->(args:{tool_args_label}) \
             RETURN DISTINCT t.a2a_event_id, t.a2a_tool_name, t.a2a_metadata, args.a2a_args, used.prov_role, args.prov_type, t.a2a_success \
             ORDER BY t.a2a_event_id"
        )
    }

    /// Typed parameterised tool query for cypher_builder().params().run().
    /// Property names use underscore form to match [crate::cypher_build::KeyStyle::StorageSafeUnderscore].
    pub fn tool_query_storage_safe_params(context: &str) -> (&'static str, serde_json::Value) {
        const QUERY: &str = "MATCH (t:ToolCall) WHERE t.a2a_context_id = $context \
             MATCH (t)-[used:WAS_USED_BY]->(args:ToolArgs) \
             RETURN DISTINCT t.a2a_event_id, t.a2a_tool_name, t.a2a_metadata, args.a2a_args, used.prov_role, args.prov_type, t.a2a_success \
             ORDER BY t.a2a_event_id";
        let params = serde_json::json!({ "context": context });
        (QUERY, params)
    }
}
