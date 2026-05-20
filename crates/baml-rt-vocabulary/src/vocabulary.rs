// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
    pub const INTENT_ID: &str = "a2a:intent_id";
    pub const PLAN_ID: &str = "a2a:plan_id";
    pub const STEP_ID: &str = "a2a:step_id";
    pub const STEP_ORDER: &str = "a2a:step_order";
    pub const DEPENDS_ON: &str = "a2a:depends_on";
    pub const STATUS: &str = "a2a:status";
    pub const AGENT_ID: &str = "a2a:agent_id";
    pub const AGENT_TYPE: &str = "a2a:agent_type";
    pub const AGENT_VERSION: &str = "a2a:agent_version";
    pub const TASK_ID: &str = "a2a:task_id";
    pub const TASK_STATE: &str = "a2a:task_state";
    pub const TASK_STATE_TIME: &str = "a2a:task_state_time";
    pub const OLD_STATUS: &str = "a2a:old_status";
    pub const INPUT_REQUIRED_PROMPT: &str = "a2a:input_required_prompt";
    pub const OLD_INPUT_REQUIRED_PROMPT: &str = "a2a:old_input_required_prompt";
    pub const MESSAGE_ID: &str = "a2a:message_id";
    pub const ROLE: &str = "a2a:role";
    pub const CONTENT: &str = "a2a:content";
    pub const DIRECTION: &str = "a2a:direction";
    /// Who spoke on a user transcript row (`human` | `relay` | `ingress`). See `user_speaker_kinds`.
    pub const USER_SPEAKER_KIND: &str = "a2a:user_speaker_kind";
    pub const METADATA: &str = "a2a:metadata";
    pub const PHASE: &str = "a2a:phase";
    pub const RESULT: &str = "a2a:result";
    pub const ERROR: &str = "a2a:error";
    pub const ACTIVITY_ANCHOR: &str = "a2a:activity_anchor";
    pub const RELATION: &str = "a2a:relation";
    pub const FROM: &str = "a2a:from";
    pub const TO: &str = "a2a:to";
    pub const CLIENT: &str = "a2a:client";
    pub const MODEL: &str = "a2a:model";
    pub const FUNCTION_NAME: &str = "a2a:function_name";
    /// The logical prompt name (base name without FSM phase suffix). Stable identity for display and config.
    pub const PROMPT_NAME: &str = "a2a:prompt_name";
    pub const PROMPT: &str = "a2a:prompt";
    /// UTF-8 length of JSON-serialized prompt (`serde_json::to_string`) measured once at emission.
    pub const PROMPT_SERIALIZED_UTF8_BYTES: &str = "a2a:prompt_serialized_utf8_bytes";
    /// Unicode scalar count of textual chat `messages` content (see prompt projection / display helpers).
    pub const PROMPT_MESSAGE_CHARS: &str = "a2a:prompt_message_chars";
    pub const USAGE_PROMPT_TOKENS: &str = "a2a:usage_prompt_tokens";
    pub const USAGE_COMPLETION_TOKENS: &str = "a2a:usage_completion_tokens";
    pub const USAGE_TOTAL_TOKENS: &str = "a2a:usage_total_tokens";
    pub const USAGE_CACHED_INPUT_TOKENS: &str = "a2a:usage_cached_input_tokens";
    pub const DURATION_MS: &str = "a2a:duration_ms";
    pub const DRIFT_SCORE: &str = "a2a:drift_score";
    pub const DRIFT_SEVERITY: &str = "a2a:drift_severity";
    pub const DRIFT_MODE: &str = "a2a:drift_mode";
    pub const DRIFT_WARN_MIN_SCORE: &str = "a2a:drift_warn_min_score";
    pub const DRIFT_BLOCK_MIN_SCORE: &str = "a2a:drift_block_min_score";
    pub const INTENT_TEXT_PREVIEW: &str = "a2a:intent_text_preview";
    pub const RESPONSE_TEXT_PREVIEW: &str = "a2a:response_text_preview";
    pub const STEP_TEXT_PREVIEW: &str = "a2a:step_text_preview";
    pub const PLAN_DRIFT_INTENT_ALIGNMENT: &str = "a2a:plan_drift_intent_alignment";
    pub const PLAN_DRIFT_STEP_ALIGNMENT: &str = "a2a:plan_drift_step_alignment";
    pub const PLAN_DRIFT_TRAJECTORY: &str = "a2a:plan_drift_trajectory";
    pub const PLAN_DRIFT_ADHERENCE: &str = "a2a:plan_drift_adherence";
    pub const PLAN_DRIFT_COMPOSITE_SEVERITY: &str = "a2a:plan_drift_composite_severity";
    pub const PLAN_DRIFT_CROSS_ENCODER_STEP: &str = "a2a:plan_drift_cross_encoder_step";
    /// JSON blob: citation-grounded drift (`LlmCitationDriftInfo`) when present.
    pub const CITATION_DRIFT: &str = "a2a:citation_drift";
    /// Cosine similarity between the previous intent embedding and the new one
    /// at a supersession boundary. Present only on revised IntentResolved events.
    pub const REVISION_INTENT_DRIFT: &str = "a2a:revision_intent_drift";
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
    pub const OLD_REASON: &str = "a2a:old_reason";
    pub const TIMESTAMP_MS: &str = "a2a:timestamp_ms";
    /// Monotonic event counter parsed from the activity anchor at write time.
    pub const EVENT_ORDER: &str = "a2a:event_order";
    /// Host ingress transcript discriminator (`source_poll_recorded` | `dispatch_accepted`).
    pub const HOST_INGRESS_KIND: &str = "a2a:host_ingress_kind";
    /// Host dispatch target (`HostDispatchAccepted`).
    pub const HOST_INGRESS_TARGET_PACKAGE: &str = "a2a:host_ingress_target_package";
    pub const HOST_INGRESS_TARGET_INSTANCE: &str = "a2a:host_ingress_target_instance";
    /// Host source poll identity (`HostSourcePollRecorded`).
    pub const HOST_INGRESS_SOURCE_KIND: &str = "a2a:host_ingress_source_kind";
    pub const HOST_INGRESS_SOURCE_KEY: &str = "a2a:host_ingress_source_key";
    pub const HOST_INGRESS_RECORD_COUNT: &str = "a2a:host_ingress_record_count";
    /// Routing key on host dispatch (`HostDispatchAccepted`).
    pub const HOST_INGRESS_ROUTING_KEY: &str = "a2a:host_ingress_routing_key";
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

/// On-disk values stored in `props.a2a_activity_outcome` (see
/// `a2a::ACTIVITY_OUTCOME` for the property key). Centralised so reads
/// and writes never disagree on capitalisation.
pub mod activity_outcome {
    /// Activity completed without error.
    pub const SUCCESS: &str = "Success";
    /// Activity completed with a recorded failure.
    pub const FAILURE: &str = "Failed";
    /// Activity reached a terminal state but the outcome could not be
    /// determined.
    pub const INDETERMINATE: &str = "Indeterminate";
    /// Activity has a start but no end time yet (still running or never
    /// finalised).
    pub const IN_PROGRESS: &str = "InProgress";
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
    pub const INTENT: &str = "a2a:Intent";
    pub const PLAN: &str = "a2a:Plan";
    pub const PLAN_STEP: &str = "a2a:PlanStep";
    pub const LLM_CALL: &str = "a2a:LlmCall";
    pub const TOOL_CALL: &str = "a2a:ToolCall";
    pub const AGENT_BOOT: &str = "a2a:AgentBoot";
    pub const AGENT_STOP: &str = "a2a:AgentStop";
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
    pub const SESSION_STEP: &str = "a2a:SessionStep";
    pub const FAILURE_CLASSIFICATION_ACTIVITY: &str = "a2a:FailureClassificationActivity";
    pub const FAILURE_CLASSIFICATION: &str = "a2a:FailureClassification";
    pub const CONTEXT: &str = "a2a:Context";
}

pub mod a2a_relation_types {
    pub const STATUS_TRANSITION: &str = "a2a:status_transition";
    /// `WasDerivedFrom.prov_type` for archive (`@N`) citation lineage: decision grounded in observed data.
    pub const INFORMED_BY_OBSERVATION: &str = "a2a:informed_by_observation";
    /// `WAS_INFORMED_BY` edge: session `SendDone` step grounded in a specific `ToolCall` completion’s result.
    pub const INFORMED_BY_TOOL_INVOCATION: &str = "a2a:informed_by_tool_invocation";
    /// `CITED` edge: LLM decision cited a specific evidence source.
    pub const CITED: &str = "a2a:cited";
}

/// Structural relation: Context node to nodes scoped to that context.
/// Used for indexed traversal instead of property-based filtering.
pub mod context_scope {
    pub const LABEL: &str = "Context";
    pub const SCOPED_TO: &str = "SCOPED_TO";

    /// Node labels that must NOT get SCOPED_TO edges. They cross context boundaries
    /// (e.g. AgentBoot/AgentArchive are shared across conversations).
    /// AgentRuntimeInstance gets SCOPED_TO so export can traverse TaskExecution -[WAS_EXECUTED_BY]-> AgentRuntimeInstance.
    pub const SCOPE_EXEMPT_LABELS: &[&str] = &["AgentBoot", "AgentArchive", "AgentStop"];
}

pub mod semantic_labels {
    pub const WAS_USED_BY: &str = "WAS_USED_BY";
    /// Failed LLM/tool call activity → `FailureClassification` entity (distinct from generic `WAS_USED_BY` usage edges).
    pub const WAS_CLASSIFIED_BY: &str = "WAS_CLASSIFIED_BY";
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
    /// Head-pointer edge `Task -> TaskState` naming the most-recent
    /// `TaskState` along the immutable `WAS_TRANSITIONED_FROM` chain.
    /// Re-pointed atomically by the normalizer on every `TaskStatusChanged`;
    /// cardinality (one per Task) is enforced by a UNIQUE index on
    /// `(rel_type, from_id)` filtered to this rel_type.
    pub const WAS_LAST_TRANSITIONED_TO: &str = "WAS_LAST_TRANSITIONED_TO";
    /// Head-pointer edge `Task -> AgentRuntimeInstance` naming the
    /// most-recent execution-owning agent. The chain edge
    /// `WAS_EXECUTED_BY` (`TaskExecution -> AgentRuntimeInstance`) carries
    /// per-execution history; this edge collapses agent-identity lookup
    /// from a multi-hop traversal to a single indexed edge hop and
    /// obsoletes the application-level `task_agent_id_cache`.
    pub const WAS_LAST_EXECUTED_BY: &str = "WAS_LAST_EXECUTED_BY";
    /// Archive / observation citation lineage (contrast `prov_relations::WAS_DERIVED_FROM` for `#N` intent history).
    pub const WAS_INFORMED_BY: &str = "WAS_INFORMED_BY";
    pub const WAS_REPLACED_BY: &str = "WAS_REPLACED_BY";
    pub const WAS_REFINED_BY: &str = "WAS_REFINED_BY";
    pub const WAS_RELATED_TO: &str = "WAS_RELATED_TO";
    pub const WAS_DELEGATED_TO: &str = "WAS_DELEGATED_TO";
    /// Detached `system/callback` dispatch task was scheduled from a parent A2A scheduling task.
    pub const WAS_SCHEDULED_FROM: &str = "WAS_SCHEDULED_FROM";
    // The canonical task↔message edge is `a2a_relations::TASK_MESSAGE`
    // (`A2A_TASK_MESSAGE`) with a `direction` attribute distinguishing
    // received-vs-sent. No separate `TASK_TRIGGERED_BY_MESSAGE` /
    // `TASK_EMITTED_MESSAGE` labels exist on disk.
    /// LLM decision cited a specific evidence source (`#N` history or `@N` archive).
    pub const CITED: &str = "CITED";
    pub const HAS_INTENT: &str = "HAS_INTENT";
    pub const HAS_PLAN: &str = "HAS_PLAN";
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

/// Wire values for [`a2a::USER_SPEAKER_KIND`](a2a::USER_SPEAKER_KIND) on user transcript rows.
pub mod user_speaker_kinds {
    pub const HUMAN: &str = "human";
    pub const RELAY: &str = "relay";
    pub const INGRESS: &str = "ingress";
}

pub mod a2a_relations {
    // Relation types whose DB rel_type is from this module (used directly by write-batch dynamic arm
    // and read paths). Keep in sync with `A2aRelationType::as_str()` in normalizer.rs.
    pub const PLAN_STEP: &str = "A2A_PLAN_STEP";
    pub const TASK_MESSAGE: &str = "A2A_TASK_MESSAGE";
    /// `A2ATask` entity → `SessionStep` entity (task-scoped tool session rows).
    pub const TASK_SESSION_STEP: &str = "A2A_TASK_SESSION_STEP";
    pub const TASK_ARTIFACT: &str = "A2A_TASK_ARTIFACT";
    pub const TASK_CALL: &str = "A2A_TASK_CALL";
    pub const TASK_STATUS_TRANSITION: &str = "A2A_TASK_STATUS_TRANSITION";
    pub const MESSAGE_CALL: &str = "A2A_MESSAGE_CALL";
    /// Reserved — `InformedByObservation` variant is not yet emitted;
    /// this value is used as the `prov_type` attribute on WAS_DERIVED_FROM edges when it is.
    pub const INFORMED_BY_OBSERVATION: &str = "A2A_INFORMED_BY_OBSERVATION";
}

pub mod graph {
    pub const NODE_ID: &str = "id";
}

pub mod storage_safe {
    pub const A2A_INTENT_ID: &str = "a2a_intent_id";
    pub const A2A_PLAN_ID: &str = "a2a_plan_id";
    pub const A2A_STEP_ID: &str = "a2a_step_id";
    pub const A2A_STEP_ORDER: &str = "a2a_step_order";
    pub const A2A_DEPENDS_ON: &str = "a2a_depends_on";
    pub const A2A_STATUS: &str = "a2a_status";
    pub const PROV_LABEL: &str = "prov_label";
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
    pub const A2A_INPUT_REQUIRED_PROMPT: &str = "a2a_input_required_prompt";
    pub const A2A_OLD_INPUT_REQUIRED_PROMPT: &str = "a2a_old_input_required_prompt";
    pub const A2A_REASON: &str = "a2a_reason";
    pub const A2A_OLD_REASON: &str = "a2a_old_reason";
    pub const A2A_MESSAGE_ID: &str = "a2a_message_id";
    pub const A2A_ROLE: &str = "a2a_role";
    pub const A2A_CONTENT: &str = "a2a_content";
    pub const A2A_DIRECTION: &str = "a2a_direction";
    pub const A2A_METADATA: &str = "a2a_metadata";
    pub const A2A_PHASE: &str = "a2a_phase";
    pub const A2A_RESULT: &str = "a2a_result";
    pub const A2A_ERROR: &str = "a2a_error";
    pub const A2A_ACTIVITY_ANCHOR: &str = "a2a_activity_anchor";
    pub const A2A_RELATION: &str = "a2a_relation";
    pub const A2A_FROM: &str = "a2a_from";
    pub const A2A_TO: &str = "a2a_to";
    pub const A2A_CLIENT: &str = "a2a_client";
    pub const A2A_MODEL: &str = "a2a_model";
    pub const A2A_FUNCTION_NAME: &str = "a2a_function_name";
    pub const A2A_PROMPT_NAME: &str = "a2a_prompt_name";
    pub const A2A_PROMPT: &str = "a2a_prompt";
    /// UTF-8 byte length of JSON-serialized LLM prompt payload on `LlmCall` nodes (single measurement at emission).
    pub const A2A_PROMPT_SERIALIZED_UTF8_BYTES: &str = "a2a_prompt_serialized_utf8_bytes";
    /// Unicode scalar count of chat message text in the LLM request (same rules as display flattening).
    pub const A2A_PROMPT_MESSAGE_CHARS: &str = "a2a_prompt_message_chars";
    pub const A2A_USAGE_PROMPT_TOKENS: &str = "a2a_usage_prompt_tokens";
    pub const A2A_USAGE_COMPLETION_TOKENS: &str = "a2a_usage_completion_tokens";
    pub const A2A_USAGE_TOTAL_TOKENS: &str = "a2a_usage_total_tokens";
    pub const A2A_USAGE_CACHED_INPUT_TOKENS: &str = "a2a_usage_cached_input_tokens";
    pub const A2A_DURATION_MS: &str = "a2a_duration_ms";
    pub const A2A_DRIFT_SCORE: &str = "a2a_drift_score";
    pub const A2A_DRIFT_SEVERITY: &str = "a2a_drift_severity";
    pub const A2A_DRIFT_MODE: &str = "a2a_drift_mode";
    pub const A2A_DRIFT_WARN_MIN_SCORE: &str = "a2a_drift_warn_min_score";
    pub const A2A_DRIFT_BLOCK_MIN_SCORE: &str = "a2a_drift_block_min_score";
    pub const A2A_INTENT_TEXT_PREVIEW: &str = "a2a_intent_text_preview";
    pub const A2A_RESPONSE_TEXT_PREVIEW: &str = "a2a_response_text_preview";
    pub const A2A_STEP_TEXT_PREVIEW: &str = "a2a_step_text_preview";
    pub const A2A_PLAN_DRIFT_INTENT_ALIGNMENT: &str = "a2a_plan_drift_intent_alignment";
    pub const A2A_PLAN_DRIFT_STEP_ALIGNMENT: &str = "a2a_plan_drift_step_alignment";
    pub const A2A_PLAN_DRIFT_TRAJECTORY: &str = "a2a_plan_drift_trajectory";
    pub const A2A_PLAN_DRIFT_ADHERENCE: &str = "a2a_plan_drift_adherence";
    pub const A2A_PLAN_DRIFT_COMPOSITE_SEVERITY: &str = "a2a_plan_drift_composite_severity";
    pub const A2A_PLAN_DRIFT_CROSS_ENCODER_STEP: &str = "a2a_plan_drift_cross_encoder_step";
    pub const A2A_CITATION_DRIFT: &str = "a2a_citation_drift";
    pub const A2A_ACTIVITY_OUTCOME: &str = "a2a_activity_outcome";
    pub const A2A_TOOL_NAME: &str = "a2a_tool_name";
    pub const A2A_ARGS: &str = "a2a_args";
    pub const A2A_ARCHIVE_PATH: &str = "a2a_archive_path";
    pub const A2A_ARTIFACT_ID: &str = "a2a_artifact_id";
    pub const A2A_ARTIFACT_TYPE: &str = "a2a_artifact_type";
    pub const A2A_CONTEXT_ID: &str = "a2a_context_id";
    pub const A2A_TIMESTAMP_MS: &str = "a2a_timestamp_ms";
    pub const A2A_EVENT_ORDER: &str = "a2a_event_order";
    pub const A2A_FAILURE_CLASS: &str = "a2a_failure_class";
    pub const A2A_FAILURE_EVIDENCE: &str = "a2a_failure_evidence";
    pub const A2A_FAILURE_CODE: &str = "a2a_failure_code";
    pub const A2A_LLM_CALL_PAYLOAD_ID: &str = "a2a_llm_call_payload_id";
    pub const A2A_LLM_RESULT_PAYLOAD_ID: &str = "a2a_llm_result_payload_id";
    pub const A2A_TOOL_CALL_PAYLOAD_ID: &str = "a2a_tool_call_payload_id";
    pub const A2A_TOOL_RESULT_PAYLOAD_ID: &str = "a2a_tool_result_payload_id";
}

pub mod node_labels {
    pub const INTENT: &str = "Intent";
    pub const PLAN: &str = "Plan";
    pub const PLAN_STEP: &str = "PlanStep";
    pub const LLM_CALL: &str = "LlmCall";
    pub const TOOL_CALL: &str = "ToolCall";
    pub const AGENT_BOOT: &str = "AgentBoot";
    pub const AGENT_STOP: &str = "AgentStop";
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

#[cfg(test)]
mod tests {
    use super::context_scope::SCOPE_EXEMPT_LABELS;

    #[test]
    fn agent_stop_is_scope_exempt() {
        assert!(
            SCOPE_EXEMPT_LABELS.contains(&"AgentStop"),
            "AgentStop nodes have no context_id and must be scope-exempt"
        );
    }
}
