//! Task executing-agent binding — canonical write-side ceremony for `WAS_LAST_EXECUTED_BY`.
//!
//! ## Invariants
//!
//! - **I-BIND-EXISTS:** If a task has agent-attributed execution with a non-nil executing agent,
//!   `Task` has exactly one `WAS_LAST_EXECUTED_BY` → booted `AgentRuntimeInstance`.
//! - **I-BIND-BEFORE-ATTRIBUTION:** First non-nil agent attribution is preceded by or co-emitted
//!   with binding in the provisioner call chain (or same normalize batch via defense-in-depth).
//! - **I-EPISODE-BOUND:** Every successfully assembled `Episode` has a non-nil `agent_id` via `WAS_LAST_EXECUTED_BY`.
//! - **I-EPISODE-GATE:** `EpisodeReader` returns `EpisodeUnbound` when the head pointer is absent — never a partial episode.
//! - **I-HOST-POLL-EXCEPTION:** Poll ingress with nil agent never creates executing-agent binding.

use baml_rt_core::ids::{AgentId, ContextId, TaskId, UuidId};
use uuid::Uuid;

use crate::{
    error::{ProvenanceError, Result},
    events::{ProvEvent, ProvEventData},
};

/// Provenance-only trace of which scope-establishment path emitted binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAgentBindingSource {
    A2aStreamBootstrap,
    HostWithTaskPrelude,
    HostDispatchInvocation,
    ToolMintedScope,
    CallbackLink,
}

/// Typed operation binding a task to its executing booted agent via `TaskExists` +
/// `TaskExecutionStarted` (which repoints `WAS_LAST_EXECUTED_BY`).
#[derive(Debug, Clone)]
pub struct TaskAgentBinding {
    pub context_id: ContextId,
    pub task_id: TaskId,
    pub executing_agent_id: AgentId,
    pub source: TaskAgentBindingSource,
}

/// Returns true when `agent_id` is the nil host sentinel (poll ingress speaker).
pub fn is_unassigned_executing_agent(agent_id: &AgentId) -> bool {
    agent_id.as_str() == AgentId::from_uuid(UuidId::new(Uuid::nil())).as_str()
}

impl TaskAgentBinding {
    pub fn new(
        context_id: ContextId,
        task_id: TaskId,
        executing_agent_id: AgentId,
        source: TaskAgentBindingSource,
    ) -> Result<Self> {
        if is_unassigned_executing_agent(&executing_agent_id) {
            return Err(ProvenanceError::InvalidEvent {
                activity_anchor: "task_agent_binding".to_string(),
                reason: "executing_agent_id must not be the nil host sentinel".to_string(),
            });
        }
        Ok(Self {
            context_id,
            task_id,
            executing_agent_id,
            source,
        })
    }

    pub fn into_events(self) -> [ProvEvent; 2] {
        [
            ProvEvent::task_exists(self.context_id.clone(), self.task_id.clone()),
            ProvEvent::task_execution_started(
                self.context_id,
                self.task_id,
                self.executing_agent_id,
            ),
        ]
    }
}

/// Agent id carried on this event when graph head pointer is not yet visible to the writer.
pub fn event_local_executing_agent_id(event: &ProvEvent) -> Option<AgentId> {
    if let Some(agent_id) = event.message_agent_id()
        && !is_unassigned_executing_agent(agent_id)
    {
        return Some(agent_id.clone());
    }
    match event.data() {
        ProvEventData::TaskExecutionStarted { agent_id, .. } => {
            if is_unassigned_executing_agent(agent_id) {
                None
            } else {
                Some(agent_id.clone())
            }
        }
        ProvEventData::LlmCallStarted { metadata, .. }
        | ProvEventData::LlmCallCompleted { metadata, .. }
        | ProvEventData::ToolCallStarted { metadata, .. }
        | ProvEventData::ToolCallCompleted { metadata, .. } => metadata
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .and_then(|s| UuidId::parse_str(s).ok())
            .map(AgentId::from_uuid)
            .filter(|id| !is_unassigned_executing_agent(id)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::ExternalId;

    use super::*;

    fn test_agent_id() -> AgentId {
        AgentId::from_uuid(UuidId::new(Uuid::new_v4()))
    }

    #[test]
    fn rejects_nil_executing_agent() {
        let err = TaskAgentBinding::new(
            ContextId::new(1, 1),
            TaskId::from_external(ExternalId::new("task-1".to_string())),
            AgentId::from_uuid(UuidId::new(Uuid::nil())),
            TaskAgentBindingSource::HostWithTaskPrelude,
        )
        .expect_err("nil agent must be rejected");
        assert!(matches!(err, ProvenanceError::InvalidEvent { .. }));
    }

    #[test]
    fn into_events_emits_exists_and_started() {
        let binding = TaskAgentBinding::new(
            ContextId::new(1, 1),
            TaskId::from_external(ExternalId::new("task-1".to_string())),
            test_agent_id(),
            TaskAgentBindingSource::A2aStreamBootstrap,
        )
        .expect("valid binding");
        let events = binding.into_events();
        assert!(events[0].task_id().is_some());
        assert!(events[1].task_id().is_some());
    }
}
