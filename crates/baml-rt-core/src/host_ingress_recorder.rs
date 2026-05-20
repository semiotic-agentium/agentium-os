//! Host-boundary provenance recording (implemented by the runner, not task-daemon).

use async_trait::async_trait;

use crate::{
    AgentDispatchRequest, ProducedEvent, Result,
    context::RuntimeScope,
    dispatch_ingress::{DispatchWorkUnit, WithTaskPrelude},
    host_source_records_body::IngressPollBody,
    ids::{AgentId, MessageId},
};

/// Reference to the host-written poll batch user line (`#1` on publish context).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressPollUserMessageRef {
    pub message_id: MessageId,
}

/// Records host ingress lineage into the provenance store (DB-first at publish/dispatch).
#[async_trait]
pub trait HostIngressRecorder: Send + Sync {
    /// Persist poll ingestion before subscriber fan-out.
    async fn record_source_poll(&self, event: &ProducedEvent) -> Result<()>;

    /// Write actionable poll batch as one `user` message (idempotent per poll batch message id).
    async fn record_ingress_poll_user_message(
        &self,
        event: &ProducedEvent,
        body: &IngressPollBody,
    ) -> Result<IngressPollUserMessageRef>;

    /// Fork unit scope, format `records`, write task-scoped unit `#1`, then return scope for agent fn.
    async fn with_task_prelude(
        &self,
        parent: &RuntimeScope,
        agent_id: AgentId,
        unit: DispatchWorkUnit,
    ) -> Result<WithTaskPrelude>;

    /// Persist dispatch acceptance after the agent returns `accepted`.
    async fn record_dispatch_accepted(
        &self,
        request: &AgentDispatchRequest,
        agent_package: &str,
        agent_instance: &str,
    ) -> Result<()>;
}
