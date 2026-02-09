//! Provenance vocabulary constants.
//!
//! This module defines all vocabulary terms used in provenance tracking,
//! following W3C PROV and A2A conventions.

// PROV standard attributes
pub mod prov {
    pub const TYPE: &str = "prov:type";
    pub const ROLE: &str = "prov:role";
    pub const LABEL: &str = "prov:label";
    pub const VALUE: &str = "prov:value";
    pub const TIME: &str = "prov:time";
    pub const ACTIVITY: &str = "prov:activity";
    pub const START_TIME: &str = "prov:startTime";
    pub const END_TIME: &str = "prov:endTime";
    // Internal extension used for compact graph queries.
    pub const BASE_TYPE: &str = "prov:base_type";
}

// A2A-specific attributes
pub mod a2a {
    // Agent attributes
    pub const AGENT_ID: &str = "a2a:agent_id";
    pub const AGENT_TYPE: &str = "a2a:agent_type";
    pub const AGENT_VERSION: &str = "a2a:agent_version";

    // Task attributes
    pub const TASK_ID: &str = "a2a:task_id";
    pub const TASK_STATE: &str = "a2a:task_state";
    pub const TASK_STATE_TIME: &str = "a2a:task_state_time";
    pub const OLD_STATUS: &str = "a2a:old_status";
    pub const IS_PREVIOUS: &str = "a2a:is_previous";

    // Message attributes
    pub const MESSAGE_ID: &str = "a2a:message_id";
    pub const ROLE: &str = "a2a:role";
    pub const CONTENT: &str = "a2a:content";
    pub const DIRECTION: &str = "a2a:direction";
    pub const METADATA: &str = "a2a:metadata";
    pub const EVENT_ID: &str = "a2a:event_id";
    pub const RELATION: &str = "a2a:relation";
    pub const FROM: &str = "a2a:from";
    pub const TO: &str = "a2a:to";

    // LLM call attributes
    pub const CLIENT: &str = "a2a:client";
    pub const MODEL: &str = "a2a:model";
    pub const FUNCTION_NAME: &str = "a2a:function_name";
    pub const PROMPT: &str = "a2a:prompt";
    pub const USAGE_PROMPT_TOKENS: &str = "a2a:usage_prompt_tokens";
    pub const USAGE_COMPLETION_TOKENS: &str = "a2a:usage_completion_tokens";
    pub const USAGE_TOTAL_TOKENS: &str = "a2a:usage_total_tokens";
    pub const DURATION_MS: &str = "a2a:duration_ms";
    pub const SUCCESS: &str = "a2a:success";

    // Tool call attributes
    pub const TOOL_NAME: &str = "a2a:tool_name";
    pub const ARGS: &str = "a2a:args";

    // Archive attributes
    pub const ARCHIVE_PATH: &str = "a2a:archive_path";
    pub const ARTIFACT_ID: &str = "a2a:artifact_id";
    pub const ARTIFACT_TYPE: &str = "a2a:artifact_type";

    // Context attributes
    pub const CONTEXT_ID: &str = "a2a:context_id";
    pub const TIMESTAMP_MS: &str = "a2a:timestamp_ms";
}

// PROV types
pub mod prov_types {
    pub const ENTITY: &str = "prov:Entity";
    pub const ACTIVITY: &str = "prov:Activity";
    pub const AGENT: &str = "prov:Agent";
}

// Base types stored in `prov:base_type`
pub mod base_types {
    pub const ENTITY: &str = "ProvEntity";
    pub const ACTIVITY: &str = "ProvActivity";
    pub const AGENT: &str = "ProvAgent";
}

// PROV relations
pub mod prov_relations {
    pub const USED: &str = "USED";
    pub const WAS_GENERATED_BY: &str = "WAS_GENERATED_BY";
    pub const QUALIFIED_GENERATION: &str = "QUALIFIED_GENERATION";
    pub const WAS_ASSOCIATED_WITH: &str = "WAS_ASSOCIATED_WITH";
    pub const WAS_DERIVED_FROM: &str = "WAS_DERIVED_FROM";
}

// A2A-specific PROV types
pub mod a2a_types {
    // Activities
    pub const LLM_CALL: &str = "a2a:LlmCall";
    pub const TOOL_CALL: &str = "a2a:ToolCall";
    pub const AGENT_BOOT: &str = "a2a:AgentBoot";
    pub const TASK_EXECUTION: &str = "a2a:A2ATaskExecution";
    pub const MESSAGE_PROCESSING: &str = "a2a:A2AMessageProcessing";

    // Entities
    pub const LLM_PROMPT: &str = "a2a:LlmPrompt";
    pub const TOOL_ARGS: &str = "a2a:ToolArgs";
    pub const AGENT_ARCHIVE: &str = "a2a:AgentArchive";
    pub const AGENT_RUNTIME_INSTANCE: &str = "a2a:AgentRuntimeInstance";
    pub const TASK: &str = "a2a:A2ATask";
    pub const TASK_STATE: &str = "a2a:A2ATaskState";
    pub const MESSAGE: &str = "a2a:Message";
    pub const ARTIFACT: &str = "a2a:Artifact";
}

// A2A relation types (used in prov:type on relations)
pub mod a2a_relation_types {
    pub const STATUS_TRANSITION: &str = "a2a:status_transition";
}

// Semantic relation labels (past tense, passive voice)
pub mod semantic_labels {
    pub const WAS_USED_BY: &str = "WAS_USED_BY";
    pub const WAS_CONSUMED_BY: &str = "WAS_CONSUMED_BY";
    pub const WAS_RECEIVED_BY: &str = "WAS_RECEIVED_BY";
    pub const WAS_SPAWNED_BY: &str = "WAS_SPAWNED_BY";
    pub const WAS_UPDATED_BY: &str = "WAS_UPDATED_BY";
    pub const WAS_BOOTSTRAPPED_BY: &str = "WAS_BOOTSTRAPPED_BY";
    pub const WAS_EMITTED_BY: &str = "WAS_EMITTED_BY";
    pub const WAS_GENERATED_BY: &str = "WAS_GENERATED_BY";
    pub const WAS_CREATED_BY: &str = "WAS_CREATED_BY";
    pub const WAS_EXECUTED_BY: &str = "WAS_EXECUTED_BY";
    pub const WAS_INVOKED_BY: &str = "WAS_INVOKED_BY";
    pub const WAS_CALLED_BY: &str = "WAS_CALLED_BY";
    pub const WAS_TRANSITIONED_FROM: &str = "WAS_TRANSITIONED_FROM";
    pub const WAS_TRANSITIONED_TO: &str = "WAS_TRANSITIONED_TO";
    pub const WAS_RELATED_TO: &str = "WAS_RELATED_TO";
}

// PROV roles
pub mod prov_roles {
    pub const EXECUTING_AGENT: &str = "executing_agent";
    pub const INVOKING_AGENT: &str = "invoking_agent";
    pub const CALLING_AGENT: &str = "calling_agent";
}

// A2A-specific roles for USED relationships
pub mod a2a_roles {
    pub const PROMPT: &str = "a2a:prompt";
    pub const ARGS: &str = "a2a:args";
    pub const ARCHIVE: &str = "a2a:archive";
    pub const INPUT_MESSAGE: &str = "input_message";
    pub const TASK_STATE: &str = "task_state";
}

// Agent type constants
pub mod agent_types {
    pub const RUNNER: &str = "runner";
    pub const CLIENT: &str = "client";
}

// Message direction constants
pub mod message_directions {
    pub const RECEIVED: &str = "received";
    pub const SENT: &str = "sent";
}

// A2A derived relation types (edge labels)
pub mod a2a_relations {
    pub const TASK_MESSAGE: &str = "A2A_TASK_MESSAGE";
    pub const TASK_ARTIFACT: &str = "A2A_TASK_ARTIFACT";
    pub const TASK_CALL: &str = "A2A_TASK_CALL";
    pub const TASK_STATUS_TRANSITION: &str = "A2A_TASK_STATUS_TRANSITION";
    pub const MESSAGE_CALL: &str = "A2A_MESSAGE_CALL";
}

// Derived node labels (sanitized `prov:type` suffixes)
pub mod node_labels {
    pub const LLM_CALL: &str = "LlmCall";
    pub const TOOL_CALL: &str = "ToolCall";
    pub const AGENT_BOOT: &str = "AgentBoot";
    pub const TASK_EXECUTION: &str = "A2ATaskExecution";
    pub const MESSAGE_PROCESSING: &str = "A2AMessageProcessing";
    pub const LLM_PROMPT: &str = "LlmPrompt";
    pub const TOOL_ARGS: &str = "ToolArgs";
    pub const AGENT_ARCHIVE: &str = "AgentArchive";
    pub const AGENT_RUNTIME_INSTANCE: &str = "AgentRuntimeInstance";
    pub const TASK: &str = "A2ATask";
    pub const TASK_STATE: &str = "A2ATaskState";
    pub const MESSAGE: &str = "A2AMessage";
    pub const ARTIFACT: &str = "Artifact";
}
