//! Shared provenance vocabulary constants.
//!
//! Centralized in `baml-rt-vocabulary` so all runtime crates use one metamodel.

pub mod prov {
    pub const TYPE: &str = "prov:type";
    pub const ROLE: &str = "prov:role";
    pub const LABEL: &str = "prov:label";
    pub const VALUE: &str = "prov:value";
    pub const TIME: &str = "prov:time";
    pub const ACTIVITY: &str = "prov:activity";
    pub const START_TIME: &str = "prov:startTime";
    pub const END_TIME: &str = "prov:endTime";
    pub const BASE_TYPE: &str = "prov:base_type";
}

pub mod a2a {
    pub const AGENT_ID: &str = "a2a:agent_id";
    pub const AGENT_TYPE: &str = "a2a:agent_type";
    pub const AGENT_VERSION: &str = "a2a:agent_version";
    pub const TASK_ID: &str = "a2a:task_id";
    pub const TASK_STATE: &str = "a2a:task_state";
    pub const TASK_STATE_TIME: &str = "a2a:task_state_time";
    pub const OLD_STATUS: &str = "a2a:old_status";
    pub const MESSAGE_ID: &str = "a2a:message_id";
    pub const ROLE: &str = "a2a:role";
    pub const CONTENT: &str = "a2a:content";
    pub const DIRECTION: &str = "a2a:direction";
    pub const METADATA: &str = "a2a:metadata";
    pub const PHASE: &str = "a2a:phase";
    pub const RESULT: &str = "a2a:result";
    pub const ERROR: &str = "a2a:error";
    pub const EVENT_ID: &str = "a2a:event_id";
    pub const RELATION: &str = "a2a:relation";
    pub const FROM: &str = "a2a:from";
    pub const TO: &str = "a2a:to";
    pub const CLIENT: &str = "a2a:client";
    pub const MODEL: &str = "a2a:model";
    pub const FUNCTION_NAME: &str = "a2a:function_name";
    pub const PROMPT: &str = "a2a:prompt";
    pub const USAGE_PROMPT_TOKENS: &str = "a2a:usage_prompt_tokens";
    pub const USAGE_COMPLETION_TOKENS: &str = "a2a:usage_completion_tokens";
    pub const USAGE_TOTAL_TOKENS: &str = "a2a:usage_total_tokens";
    pub const DURATION_MS: &str = "a2a:duration_ms";
    /// Tri-state outcome inferred from (1) activity having an end time and (2) outcome.
    /// InProgress when no end time; Success | Failed when completed, from outcome.
    pub const ACTIVITY_OUTCOME: &str = "a2a:activity_outcome";
    pub const TOOL_NAME: &str = "a2a:tool_name";
    pub const ARGS: &str = "a2a:args";
    pub const ARCHIVE_PATH: &str = "a2a:archive_path";
    pub const ARTIFACT_ID: &str = "a2a:artifact_id";
    pub const ARTIFACT_TYPE: &str = "a2a:artifact_type";
    pub const CONTEXT_ID: &str = "a2a:context_id";
    pub const REASON: &str = "a2a:reason";
    pub const TIMESTAMP_MS: &str = "a2a:timestamp_ms";
    pub const DELEGATION_TARGET: &str = "a2a:delegation_target";
    pub const FAILURE_CLASS: &str = "a2a:failure_class";
    pub const FAILURE_EVIDENCE: &str = "a2a:failure_evidence";
    pub const FAILURE_CODE: &str = "a2a:failure_code";
    pub const LLM_CALL_PAYLOAD_ID: &str = "a2a:llm_call_payload_id";
    pub const LLM_RESULT_PAYLOAD_ID: &str = "a2a:llm_result_payload_id";
    pub const TOOL_CALL_PAYLOAD_ID: &str = "a2a:tool_call_payload_id";
    pub const TOOL_RESULT_PAYLOAD_ID: &str = "a2a:tool_result_payload_id";
}

pub mod prov_types {
    pub const ENTITY: &str = "prov:Entity";
    pub const ACTIVITY: &str = "prov:Activity";
    pub const AGENT: &str = "prov:Agent";
}

pub mod base_types {
    pub const ENTITY: &str = "ProvEntity";
    pub const ACTIVITY: &str = "ProvActivity";
    pub const AGENT: &str = "ProvAgent";
}

pub mod prov_relations {
    pub const USED: &str = "USED";
    pub const WAS_GENERATED_BY: &str = "WAS_GENERATED_BY";
    pub const QUALIFIED_GENERATION: &str = "QUALIFIED_GENERATION";
    pub const WAS_ASSOCIATED_WITH: &str = "WAS_ASSOCIATED_WITH";
    pub const WAS_DERIVED_FROM: &str = "WAS_DERIVED_FROM";
}

pub mod a2a_types {
    pub const LLM_CALL: &str = "a2a:LlmCall";
    pub const TOOL_CALL: &str = "a2a:ToolCall";
    pub const AGENT_BOOT: &str = "a2a:AgentBoot";
    pub const TASK_EXECUTION: &str = "a2a:A2ATaskExecution";
    pub const MESSAGE_PROCESSING: &str = "a2a:A2AMessageProcessing";
    pub const LLM_PROMPT: &str = "a2a:LlmPrompt";
    pub const PROMPT_REJECTED: &str = "a2a:PromptRejected";
    pub const TOOL_ARGS: &str = "a2a:ToolArgs";
    pub const AGENT_ARCHIVE: &str = "a2a:AgentArchive";
    pub const AGENT_RUNTIME_INSTANCE: &str = "a2a:AgentRuntimeInstance";
    pub const TASK: &str = "a2a:A2ATask";
    pub const TASK_STATE: &str = "a2a:A2ATaskState";
    pub const MESSAGE: &str = "a2a:Message";
    pub const ARTIFACT: &str = "a2a:Artifact";
    pub const DELEGATION_TARGET: &str = "a2a:DelegationTarget";
    pub const FAILURE_CLASSIFICATION_ACTIVITY: &str = "a2a:FailureClassificationActivity";
    pub const FAILURE_CLASSIFICATION: &str = "a2a:FailureClassification";
}

pub mod a2a_relation_types {
    pub const STATUS_TRANSITION: &str = "a2a:status_transition";
}

/// Structural relation: Context node to nodes scoped to that context.
/// Used for indexed traversal instead of property-based filtering.
pub mod context_scope {
    pub const LABEL: &str = "Context";
    pub const SCOPED_TO: &str = "SCOPED_TO";

    /// Node labels that must NOT get SCOPED_TO edges. They cross context boundaries
    /// (e.g. AgentBoot/AgentArchive are shared across conversations).
    /// AgentRuntimeInstance gets SCOPED_TO so export can traverse TaskExecution -[WAS_EXECUTED_BY]-> AgentRuntimeInstance.
    pub const SCOPE_EXEMPT_LABELS: &[&str] = &["AgentBoot", "AgentArchive"];
}

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
    pub const WAS_DELEGATED_TO: &str = "WAS_DELEGATED_TO";
    pub const TASK_TRIGGERED_BY_MESSAGE: &str = "TASK_TRIGGERED_BY_MESSAGE";
    pub const TASK_EMITTED_MESSAGE: &str = "TASK_EMITTED_MESSAGE";
}

pub mod prov_roles {
    pub const EXECUTING_AGENT: &str = "executing_agent";
    pub const INVOKING_AGENT: &str = "invoking_agent";
    pub const CALLING_AGENT: &str = "calling_agent";
}

pub mod a2a_roles {
    pub const PROMPT: &str = "a2a:prompt";
    pub const ARGS: &str = "a2a:args";
    pub const ARCHIVE: &str = "a2a:archive";
    pub const INPUT_MESSAGE: &str = "input_message";
    pub const REJECTED_OUTPUT: &str = "a2a:rejected_output";
    pub const TASK_STATE: &str = "task_state";
    pub const DELEGATION_TARGET: &str = "a2a:delegation_target";
    pub const FAILURE_CLASSIFICATION: &str = "a2a:failure_classification";
    pub const FAILURE_EVIDENCE: &str = "a2a:failure_evidence";
}

pub mod agent_types {
    pub const RUNNER: &str = "runner";
    pub const CLIENT: &str = "client";
}

pub mod message_directions {
    pub const RECEIVED: &str = "received";
    pub const SENT: &str = "sent";
}

pub mod a2a_relations {
    pub const TASK_MESSAGE: &str = "A2A_TASK_MESSAGE";
    pub const TASK_ARTIFACT: &str = "A2A_TASK_ARTIFACT";
    pub const TASK_CALL: &str = "A2A_TASK_CALL";
    pub const TASK_STATUS_TRANSITION: &str = "A2A_TASK_STATUS_TRANSITION";
    pub const MESSAGE_CALL: &str = "A2A_MESSAGE_CALL";
}

pub mod graph {
    pub const NODE_ID: &str = "id";
}

pub mod storage_safe {
    pub const PROV_TYPE: &str = "prov_type";
    pub const PROV_ROLE: &str = "prov_role";
    pub const PROV_BASE_TYPE: &str = "prov_base_type";
    pub const PROV_TIME: &str = "prov_time";
    pub const PROV_ACTIVITY: &str = "prov_activity";
    pub const PROV_START_TIME: &str = "prov_startTime";
    pub const PROV_END_TIME: &str = "prov_endTime";
    pub const A2A_AGENT_ID: &str = "a2a_agent_id";
    pub const A2A_AGENT_TYPE: &str = "a2a_agent_type";
    pub const A2A_AGENT_VERSION: &str = "a2a_agent_version";
    pub const A2A_TASK_ID: &str = "a2a_task_id";
    pub const A2A_TASK_STATE: &str = "a2a_task_state";
    pub const A2A_TASK_STATE_TIME: &str = "a2a_task_state_time";
    pub const A2A_OLD_STATUS: &str = "a2a_old_status";
    pub const A2A_MESSAGE_ID: &str = "a2a_message_id";
    pub const A2A_ROLE: &str = "a2a_role";
    pub const A2A_CONTENT: &str = "a2a_content";
    pub const A2A_DIRECTION: &str = "a2a_direction";
    pub const A2A_METADATA: &str = "a2a_metadata";
    pub const A2A_PHASE: &str = "a2a_phase";
    pub const A2A_RESULT: &str = "a2a_result";
    pub const A2A_ERROR: &str = "a2a_error";
    pub const A2A_EVENT_ID: &str = "a2a_event_id";
    pub const A2A_RELATION: &str = "a2a_relation";
    pub const A2A_FROM: &str = "a2a_from";
    pub const A2A_TO: &str = "a2a_to";
    pub const A2A_CLIENT: &str = "a2a_client";
    pub const A2A_MODEL: &str = "a2a_model";
    pub const A2A_FUNCTION_NAME: &str = "a2a_function_name";
    pub const A2A_PROMPT: &str = "a2a_prompt";
    pub const A2A_USAGE_PROMPT_TOKENS: &str = "a2a_usage_prompt_tokens";
    pub const A2A_USAGE_COMPLETION_TOKENS: &str = "a2a_usage_completion_tokens";
    pub const A2A_USAGE_TOTAL_TOKENS: &str = "a2a_usage_total_tokens";
    pub const A2A_DURATION_MS: &str = "a2a_duration_ms";
    pub const A2A_ACTIVITY_OUTCOME: &str = "a2a_activity_outcome";
    pub const A2A_TOOL_NAME: &str = "a2a_tool_name";
    pub const A2A_ARGS: &str = "a2a_args";
    pub const A2A_ARCHIVE_PATH: &str = "a2a_archive_path";
    pub const A2A_ARTIFACT_ID: &str = "a2a_artifact_id";
    pub const A2A_ARTIFACT_TYPE: &str = "a2a_artifact_type";
    pub const A2A_CONTEXT_ID: &str = "a2a_context_id";
    pub const A2A_TIMESTAMP_MS: &str = "a2a_timestamp_ms";
    pub const A2A_FAILURE_CLASS: &str = "a2a_failure_class";
    pub const A2A_FAILURE_EVIDENCE: &str = "a2a_failure_evidence";
    pub const A2A_FAILURE_CODE: &str = "a2a_failure_code";
    pub const A2A_LLM_CALL_PAYLOAD_ID: &str = "a2a_llm_call_payload_id";
    pub const A2A_LLM_RESULT_PAYLOAD_ID: &str = "a2a_llm_result_payload_id";
    pub const A2A_TOOL_CALL_PAYLOAD_ID: &str = "a2a_tool_call_payload_id";
    pub const A2A_TOOL_RESULT_PAYLOAD_ID: &str = "a2a_tool_result_payload_id";
}

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
    pub const DELEGATION_TARGET: &str = "DelegationTarget";
    pub const FAILURE_CLASSIFICATION_ACTIVITY: &str = "FailureClassificationActivity";
    pub const FAILURE_CLASSIFICATION: &str = "FailureClassification";
}
