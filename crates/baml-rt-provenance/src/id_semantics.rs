use baml_rt_core::ids::{AgentId, ArtifactId, ContextId, EventId, MessageId, TaskId};
use baml_rt_id::{
    ConstantConstructible, ConstantId, DerivedConstructible, DerivedId, ProvActivitySemantics,
    ProvAgentSemantics, ProvConstantAgentSemantics, ProvConstantIdTemplate,
    ProvDerivedActivitySemantics, ProvDerivedAgentSemantics, ProvDerivedEntitySemantics,
    ProvDerivedIdTemplate, ProvEntitySemantics, ProvIdSemantics, ProvKind, ProvVocabularyType,
};

use crate::vocabulary::a2a_types;

/// Provenance ID semantics for derived graph nodes.
///
/// These are **not** runtime identifiers themselves; they document how the
/// provenance graph node IDs are constructed and why that matches semantics.
///
/// Activity representing a single LLM call.
pub struct LlmCallActivityId;
impl DerivedConstructible for LlmCallActivityId {}
impl ProvIdSemantics for LlmCallActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for LlmCallActivityId {}
impl ProvDerivedActivitySemantics for LlmCallActivityId {}
impl ProvVocabularyType for LlmCallActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::LLM_CALL;
}

/// Deterministic composite from operational identifiers: (context_id, scope_id, agent_id, ordinal).
/// Scope_id is message_id (Message scope) or task_id (Task scope). Ordinal is call index within scope.
pub struct LlmCallActivityInput<'a> {
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for LlmCallActivityId {
    type Input<'a> = LlmCallActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("llm_call", [input.scope_key, &input.ordinal.to_string()])
    }
}

/// Entity representing an LLM prompt payload.
pub struct LlmPromptEntityId;
impl DerivedConstructible for LlmPromptEntityId {}
impl ProvIdSemantics for LlmPromptEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for LlmPromptEntityId {}
impl ProvDerivedEntitySemantics for LlmPromptEntityId {}
impl ProvVocabularyType for LlmPromptEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::LLM_PROMPT;
}

/// Same composite as LlmCallActivityId: one prompt entity per LLM call.
pub struct LlmPromptEntityInput<'a> {
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for LlmPromptEntityId {
    type Input<'a> = LlmPromptEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("llm_prompt", [input.scope_key, &input.ordinal.to_string()])
    }
}

/// Activity representing an instantaneous rejection of an LLM prompt's output (e.g. plan extraction failure).
pub struct PromptRejectedActivityId;
impl DerivedConstructible for PromptRejectedActivityId {}
impl ProvIdSemantics for PromptRejectedActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for PromptRejectedActivityId {}
impl ProvDerivedActivitySemantics for PromptRejectedActivityId {}
impl ProvVocabularyType for PromptRejectedActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::PROMPT_REJECTED;
}

/// Same composite as LlmCallActivityId: one rejection per LLM call (rejects that call's output).
pub struct PromptRejectedActivityInput<'a> {
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for PromptRejectedActivityId {
    type Input<'a> = PromptRejectedActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "prompt_rejected",
            [input.scope_key, &input.ordinal.to_string()],
        )
    }
}

/// Activity representing write-time canonical failure classification for a call activity.
pub struct FailureClassificationActivityId;
impl DerivedConstructible for FailureClassificationActivityId {}
impl ProvIdSemantics for FailureClassificationActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for FailureClassificationActivityId {}
impl ProvDerivedActivitySemantics for FailureClassificationActivityId {}
impl ProvVocabularyType for FailureClassificationActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::FAILURE_CLASSIFICATION_ACTIVITY;
}

pub struct FailureClassificationActivityInput<'a> {
    pub call_kind: &'a str,
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for FailureClassificationActivityId {
    type Input<'a> = FailureClassificationActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "failure_classification_activity",
            [input.call_kind, input.scope_key, &input.ordinal.to_string()],
        )
    }
}

/// Entity representing canonical failure classification payload for a call activity.
pub struct FailureClassificationEntityId;
impl DerivedConstructible for FailureClassificationEntityId {}
impl ProvIdSemantics for FailureClassificationEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for FailureClassificationEntityId {}
impl ProvDerivedEntitySemantics for FailureClassificationEntityId {}
impl ProvVocabularyType for FailureClassificationEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::FAILURE_CLASSIFICATION;
}

pub struct FailureClassificationEntityInput<'a> {
    pub call_kind: &'a str,
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for FailureClassificationEntityId {
    type Input<'a> = FailureClassificationEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "failure_classification",
            [input.call_kind, input.scope_key, &input.ordinal.to_string()],
        )
    }
}

/// Activity representing a single tool invocation.
pub struct ToolCallActivityId;
impl DerivedConstructible for ToolCallActivityId {}
impl ProvIdSemantics for ToolCallActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for ToolCallActivityId {}
impl ProvDerivedActivitySemantics for ToolCallActivityId {}
impl ProvVocabularyType for ToolCallActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::TOOL_CALL;
}

/// Deterministic composite from operational identifiers: (context_id, scope_id, agent_id, ordinal).
pub struct ToolCallActivityInput<'a> {
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for ToolCallActivityId {
    type Input<'a> = ToolCallActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("tool_call", [input.scope_key, &input.ordinal.to_string()])
    }
}

/// Entity representing tool arguments payload.
pub struct ToolArgsEntityId;
impl DerivedConstructible for ToolArgsEntityId {}
impl ProvIdSemantics for ToolArgsEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for ToolArgsEntityId {}
impl ProvDerivedEntitySemantics for ToolArgsEntityId {}
impl ProvVocabularyType for ToolArgsEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::TOOL_ARGS;
}

/// Same composite as ToolCallActivityId: one args entity per tool call.
pub struct ToolArgsEntityInput<'a> {
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for ToolArgsEntityId {
    type Input<'a> = ToolArgsEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("tool_args", [input.scope_key, &input.ordinal.to_string()])
    }
}

/// Entity representing the delegated-to agent for system/internal_a2a (write-time provenance).
pub struct DelegationTargetEntityId;
impl DerivedConstructible for DelegationTargetEntityId {}
impl ProvIdSemantics for DelegationTargetEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for DelegationTargetEntityId {}
impl ProvDerivedEntitySemantics for DelegationTargetEntityId {}
impl ProvVocabularyType for DelegationTargetEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::DELEGATION_TARGET;
}

/// Same composite as ToolCallActivityId: one delegation target per tool call.
pub struct DelegationTargetEntityInput<'a> {
    pub scope_key: &'a str,
    pub ordinal: u64,
}

impl ProvDerivedIdTemplate for DelegationTargetEntityId {
    type Input<'a> = DelegationTargetEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "delegation_target",
            [input.scope_key, &input.ordinal.to_string()],
        )
    }
}

/// Entity representing a task.
pub struct TaskEntityId;
impl DerivedConstructible for TaskEntityId {}
impl ProvIdSemantics for TaskEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for TaskEntityId {}
impl ProvDerivedEntitySemantics for TaskEntityId {}
impl ProvVocabularyType for TaskEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::TASK;
}

pub struct TaskEntityInput<'a> {
    pub task_id: &'a TaskId,
}

impl ProvDerivedIdTemplate for TaskEntityId {
    type Input<'a> = TaskEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("task", [input.task_id.as_str()])
    }
}

/// Entity representing a resolved intent within a task scope.
pub struct IntentEntityId;
impl DerivedConstructible for IntentEntityId {}
impl ProvIdSemantics for IntentEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for IntentEntityId {}
impl ProvDerivedEntitySemantics for IntentEntityId {}
impl ProvVocabularyType for IntentEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::INTENT;
}

pub struct IntentEntityInput<'a> {
    pub task_id: &'a TaskId,
    pub intent_id: &'a str,
}

impl ProvDerivedIdTemplate for IntentEntityId {
    type Input<'a> = IntentEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("intent", [input.task_id.as_str(), input.intent_id])
    }
}

/// Entity representing a generated plan within a task scope.
pub struct PlanEntityId;
impl DerivedConstructible for PlanEntityId {}
impl ProvIdSemantics for PlanEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for PlanEntityId {}
impl ProvDerivedEntitySemantics for PlanEntityId {}
impl ProvVocabularyType for PlanEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::PLAN;
}

pub struct PlanEntityInput<'a> {
    pub task_id: &'a TaskId,
    pub plan_id: &'a str,
}

impl ProvDerivedIdTemplate for PlanEntityId {
    type Input<'a> = PlanEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("plan", [input.task_id.as_str(), input.plan_id])
    }
}

/// Entity representing a plan step within a plan.
pub struct PlanStepEntityId;
impl DerivedConstructible for PlanStepEntityId {}
impl ProvIdSemantics for PlanStepEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for PlanStepEntityId {}
impl ProvDerivedEntitySemantics for PlanStepEntityId {}
impl ProvVocabularyType for PlanStepEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::PLAN_STEP;
}

pub struct PlanStepEntityInput<'a> {
    pub task_id: &'a TaskId,
    pub plan_id: &'a str,
    pub step_id: &'a str,
}

impl ProvDerivedIdTemplate for PlanStepEntityId {
    type Input<'a> = PlanStepEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "plan_step",
            [input.task_id.as_str(), input.plan_id, input.step_id],
        )
    }
}

/// Entity representing a task state. One node per (task_id, status). Idempotent MERGE.
pub struct TaskStateEntityId;
impl DerivedConstructible for TaskStateEntityId {}
impl ProvIdSemantics for TaskStateEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for TaskStateEntityId {}
impl ProvDerivedEntitySemantics for TaskStateEntityId {}
impl ProvVocabularyType for TaskStateEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::TASK_STATE;
}

pub struct TaskStateEntityInput<'a> {
    pub task_id: &'a TaskId,
    pub status: &'a str,
}

/// Canonical TaskState entity ID. One node per (task_id, status). Idempotent MERGE.
pub fn task_state_entity_id_string(task_id: &str, status: &str) -> String {
    format!("task_state:{task_id}:{status}")
}

impl ProvDerivedIdTemplate for TaskStateEntityId {
    type Input<'a> = TaskStateEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::new(task_state_entity_id_string(
            input.task_id.as_str(),
            input.status,
        ))
    }
}

/// Activity representing execution of a task.
pub struct TaskExecutionActivityId;
impl DerivedConstructible for TaskExecutionActivityId {}
impl ProvIdSemantics for TaskExecutionActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for TaskExecutionActivityId {}
impl ProvDerivedActivitySemantics for TaskExecutionActivityId {}
impl ProvVocabularyType for TaskExecutionActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::TASK_EXECUTION;
}

pub struct TaskExecutionActivityInput<'a> {
    pub task_id: &'a TaskId,
}

/// Canonical TaskExecution activity ID. One activity per task.
pub fn task_execution_activity_id_string(task_id: &str) -> String {
    format!("task_execution_{task_id}")
}

impl ProvDerivedIdTemplate for TaskExecutionActivityId {
    type Input<'a> = TaskExecutionActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::new(task_execution_activity_id_string(input.task_id.as_str()))
    }
}

/// Agent representing a runtime instance of an agent.
pub struct AgentRuntimeInstanceId;
impl DerivedConstructible for AgentRuntimeInstanceId {}
impl ProvIdSemantics for AgentRuntimeInstanceId {
    const KIND: ProvKind = ProvKind::Agent;
}
impl ProvAgentSemantics for AgentRuntimeInstanceId {}
impl ProvDerivedAgentSemantics for AgentRuntimeInstanceId {}
impl ProvVocabularyType for AgentRuntimeInstanceId {
    const VOCAB_TYPE: &'static str = a2a_types::AGENT_RUNTIME_INSTANCE;
}

pub struct AgentRuntimeInstanceInput<'a> {
    pub agent_id: &'a AgentId,
}

impl ProvDerivedIdTemplate for AgentRuntimeInstanceId {
    type Input<'a> = AgentRuntimeInstanceInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("agent_instance", [input.agent_id.as_str()])
    }
}

/// Entity representing an artifact by explicit artifact id.
pub struct ArtifactByIdEntityId;
impl DerivedConstructible for ArtifactByIdEntityId {}
impl ProvIdSemantics for ArtifactByIdEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for ArtifactByIdEntityId {}
impl ProvDerivedEntitySemantics for ArtifactByIdEntityId {}
impl ProvVocabularyType for ArtifactByIdEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::ARTIFACT;
}

pub struct ArtifactByIdEntityInput<'a> {
    pub artifact_id: &'a ArtifactId,
}

impl ProvDerivedIdTemplate for ArtifactByIdEntityId {
    type Input<'a> = ArtifactByIdEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("artifact", [input.artifact_id.as_str()])
    }
}

/// Entity representing an artifact by task id + type.
pub struct ArtifactByTypeEntityId;
impl DerivedConstructible for ArtifactByTypeEntityId {}
impl ProvIdSemantics for ArtifactByTypeEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for ArtifactByTypeEntityId {}
impl ProvDerivedEntitySemantics for ArtifactByTypeEntityId {}
impl ProvVocabularyType for ArtifactByTypeEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::ARTIFACT;
}

pub struct ArtifactByTypeEntityInput<'a> {
    pub task_id: &'a TaskId,
    pub artifact_type: &'a str,
}

impl ProvDerivedIdTemplate for ArtifactByTypeEntityId {
    type Input<'a> = ArtifactByTypeEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("artifact", [input.task_id.as_str(), input.artifact_type])
    }
}

/// Entity representing an artifact by task id + event id.
pub struct ArtifactByEventEntityId;
impl DerivedConstructible for ArtifactByEventEntityId {}
impl ProvIdSemantics for ArtifactByEventEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for ArtifactByEventEntityId {}
impl ProvDerivedEntitySemantics for ArtifactByEventEntityId {}
impl ProvVocabularyType for ArtifactByEventEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::ARTIFACT;
}

pub struct ArtifactByEventEntityInput<'a> {
    pub task_id: &'a TaskId,
    pub event_id: &'a EventId,
}

impl ProvDerivedIdTemplate for ArtifactByEventEntityId {
    type Input<'a> = ArtifactByEventEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "artifact",
            [input.task_id.as_str(), input.event_id.as_str()],
        )
    }
}

pub enum ArtifactIdentity<'a> {
    ById(&'a ArtifactId),
    ByType {
        task_id: &'a TaskId,
        artifact_type: &'a str,
    },
    ByEvent {
        task_id: &'a TaskId,
        event_id: &'a EventId,
    },
}

/// Activity representing an agent boot.
pub struct AgentBootActivityId;
impl DerivedConstructible for AgentBootActivityId {}
impl ProvIdSemantics for AgentBootActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for AgentBootActivityId {}
impl ProvDerivedActivitySemantics for AgentBootActivityId {}
impl ProvVocabularyType for AgentBootActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::AGENT_BOOT;
}

pub struct AgentBootActivityInput<'a> {
    pub agent_id: &'a AgentId,
}

impl ProvDerivedIdTemplate for AgentBootActivityId {
    type Input<'a> = AgentBootActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts("agent_boot", [input.agent_id.as_str()])
    }
}

/// Entity representing an agent archive (package identity).
pub struct ArchiveEntityId;
impl DerivedConstructible for ArchiveEntityId {}
impl ProvIdSemantics for ArchiveEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for ArchiveEntityId {}
impl ProvDerivedEntitySemantics for ArchiveEntityId {}
impl ProvVocabularyType for ArchiveEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::AGENT_ARCHIVE;
}

pub struct ArchiveEntityInput<'a> {
    pub archive_path: &'a str,
}

fn sanitize_archive_path(path: &str) -> String {
    path.replace(['/', '\\'], "_")
}

impl ProvDerivedIdTemplate for ArchiveEntityId {
    type Input<'a> = ArchiveEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        let sanitized = sanitize_archive_path(input.archive_path);
        DerivedId::from_parts("archive", [sanitized.as_str()])
    }
}

/// Agent representing the runner's runtime instance (control plane identity).
pub struct RunnerRuntimeInstanceId;
impl ConstantConstructible for RunnerRuntimeInstanceId {}
impl ProvIdSemantics for RunnerRuntimeInstanceId {
    const KIND: ProvKind = ProvKind::Agent;
}
impl ProvAgentSemantics for RunnerRuntimeInstanceId {}
impl ProvConstantAgentSemantics for RunnerRuntimeInstanceId {}
impl ProvVocabularyType for RunnerRuntimeInstanceId {
    const VOCAB_TYPE: &'static str = a2a_types::AGENT_RUNTIME_INSTANCE;
}

impl ProvConstantIdTemplate for RunnerRuntimeInstanceId {
    fn build() -> ConstantId {
        ConstantId::new("agent:runner")
    }
}

/// Entity representing a message.
pub struct MessageEntityId;
impl DerivedConstructible for MessageEntityId {}
impl ProvIdSemantics for MessageEntityId {
    const KIND: ProvKind = ProvKind::Entity;
}
impl ProvEntitySemantics for MessageEntityId {}
impl ProvDerivedEntitySemantics for MessageEntityId {}
impl ProvVocabularyType for MessageEntityId {
    const VOCAB_TYPE: &'static str = a2a_types::MESSAGE;
}

pub struct MessageEntityInput<'a> {
    pub context_id: &'a ContextId,
    pub message_id: &'a MessageId,
}

impl ProvDerivedIdTemplate for MessageEntityId {
    type Input<'a> = MessageEntityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::from_parts(
            "message",
            [input.context_id.as_str(), input.message_id.as_str()],
        )
    }
}

/// Activity representing message processing.
pub struct MessageProcessingActivityId;
impl DerivedConstructible for MessageProcessingActivityId {}
impl ProvIdSemantics for MessageProcessingActivityId {
    const KIND: ProvKind = ProvKind::Activity;
}
impl ProvActivitySemantics for MessageProcessingActivityId {}
impl ProvDerivedActivitySemantics for MessageProcessingActivityId {}
impl ProvVocabularyType for MessageProcessingActivityId {
    const VOCAB_TYPE: &'static str = a2a_types::MESSAGE_PROCESSING;
}

pub struct MessageProcessingActivityInput<'a> {
    pub context_id: &'a ContextId,
    pub message_id: &'a MessageId,
}

impl ProvDerivedIdTemplate for MessageProcessingActivityId {
    type Input<'a> = MessageProcessingActivityInput<'a>;

    fn build<'a>(input: Self::Input<'a>) -> DerivedId {
        DerivedId::new(format!(
            "message_processing:{}:{}",
            input.context_id.as_str(),
            input.message_id.as_str()
        ))
    }
}
