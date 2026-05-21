//! Surreal-backed [`HostIngressRecorder`] for publish and dispatch `withTask` preludes.

use std::sync::Arc;

use baml_rt_core::{
    AgentDispatchRequest, BamlRtError, HostIngressRecorder, IngressPollUserMessageRef,
    ProducedEvent, Result,
    context::RuntimeScope,
    dispatch_ingress::{
        DispatchWorkUnit, WithTaskPrelude, dispatch_unit_runtime_scope, format_unit_ingress_body,
    },
    host_source_records_body::IngressPollBody,
    host_wire::wire,
    ids::{ActivityAnchorId, AgentId, MessageId, UuidId},
};
use uuid::Uuid;

use crate::{ProvEvent, ProvenanceWriter, SurrealProvenanceStore};

/// Writes host ingress poll/unit user messages and lineage events to Surreal provenance.
pub struct HostIngressRecorderImpl {
    store: Arc<SurrealProvenanceStore>,
}

impl HostIngressRecorderImpl {
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

fn host_ingress_writer_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::nil()))
}

fn ingress_poll_user_anchor(
    context_id: &baml_rt_core::ContextId,
    batch_message_id: &str,
) -> ActivityAnchorId {
    ActivityAnchorId::from(format!(
        "ingress-poll-user:{}:{}",
        context_id.as_str(),
        batch_message_id
    ))
}

fn ingress_unit_user_anchor(
    context_id: &baml_rt_core::ContextId,
    unit_key: &str,
) -> ActivityAnchorId {
    ActivityAnchorId::from(format!(
        "ingress-unit-user:{}:{}",
        context_id.as_str(),
        unit_key
    ))
}

fn build_poll_user_message_event(
    event: &ProducedEvent,
    body: &IngressPollBody,
) -> Result<ProvEvent> {
    let context_id = event
        .context_id
        .clone()
        .ok_or_else(|| BamlRtError::InvalidArgument("ProducedEvent missing context_id".into()))?;
    let batch_message_id = event
        .message_id
        .as_deref()
        .ok_or_else(|| BamlRtError::InvalidArgument("ProducedEvent missing message_id".into()))?;
    let message_id = MessageId::from(batch_message_id);
    let anchor = ingress_poll_user_anchor(&context_id, batch_message_id);
    let agent_id = host_ingress_writer_agent_id();
    let content = vec![body.0.clone()];
    let timestamp_ms =
        baml_rt_core::now_unix_ms(baml_rt_core::clock_events::HOST_INGRESS_POLL_USER);
    if let Some(task_id) = event.task_id.clone() {
        return Ok(ProvEvent::Task(crate::events::TaskScopedEvent {
            id: anchor,
            context_id,
            task_id,
            timestamp_ms,
            data: crate::events::ProvEventData::MessageReceived {
                id: message_id,
                role: "user".to_string(),
                content,
                metadata: None,
                agent_id,
                citations: Vec::new(),
            },
        }));
    }
    Ok(ProvEvent::Global(crate::events::GlobalEvent {
        id: anchor,
        context_id,
        timestamp_ms,
        data: crate::events::ProvEventData::MessageReceived {
            id: message_id.clone(),
            role: "user".to_string(),
            content,
            metadata: None,
            agent_id,
            citations: Vec::new(),
        },
    }))
}

fn build_unit_user_message_event(
    scope: &RuntimeScope,
    unit_key: &str,
    body: &IngressPollBody,
) -> ProvEvent {
    let context_id = scope.context_id().clone();
    let task_id = scope
        .task_id_opt()
        .expect("dispatch unit scope must be task-scoped")
        .clone();
    let message_id = scope.message_id().clone();
    let anchor = ingress_unit_user_anchor(&context_id, unit_key);
    ProvEvent::Task(crate::events::TaskScopedEvent {
        id: anchor,
        context_id,
        task_id,
        timestamp_ms: baml_rt_core::now_unix_ms(baml_rt_core::clock_events::HOST_INGRESS_UNIT_USER),
        data: crate::events::ProvEventData::MessageReceived {
            id: message_id,
            role: "user".to_string(),
            content: vec![body.0.clone()],
            metadata: None,
            agent_id: scope.agent_id().clone(),
            citations: Vec::new(),
        },
    })
}

#[async_trait::async_trait]
impl HostIngressRecorder for HostIngressRecorderImpl {
    async fn record_source_poll(&self, event: &ProducedEvent) -> Result<()> {
        let context_id = event.context_id.clone().ok_or_else(|| {
            BamlRtError::InvalidArgument("ProducedEvent missing context_id".into())
        })?;
        let source_cursor = event
            .metadata
            .as_ref()
            .and_then(|m| m.get("source_cursor"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| event.source_key.as_str().to_string());
        let source_message_ts = event
            .metadata
            .as_ref()
            .and_then(|m| m.get("source_message_ts"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let record_count = event
            .messages
            .first()
            .and_then(|batch| batch.get("records").and_then(|r| r.as_array()))
            .map(|a| a.len())
            .unwrap_or(0);
        let prov_event = ProvEvent::host_source_poll_recorded(
            context_id,
            event.source_kind.as_str().to_string(),
            event.source_key.as_str().to_string(),
            source_cursor,
            event.schema_version.as_str().to_string(),
            record_count,
            source_message_ts,
        );
        self.store
            .add_event_with_logging(prov_event, "host source poll recorded")
            .await;

        // Actionable ingress user lines are written per dispatch unit in `with_task_prelude`
        // (`ingress-unit-user`), not as a second global poll Message for host.source-records.v1.
        Ok(())
    }

    async fn record_ingress_poll_user_message(
        &self,
        event: &ProducedEvent,
        body: &IngressPollBody,
    ) -> Result<IngressPollUserMessageRef> {
        if body.0.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "ingress poll body must be non-empty".into(),
            ));
        }
        let prov_event = build_poll_user_message_event(event, body)?;
        let message_id = match prov_event.data() {
            crate::events::ProvEventData::MessageReceived { id, .. } => id.clone(),
            _ => unreachable!("poll user event is MessageReceived"),
        };
        self.store
            .add_event_with_logging(prov_event, "host ingress poll user message")
            .await;
        Ok(IngressPollUserMessageRef { message_id })
    }

    async fn with_task_prelude(
        &self,
        parent: &RuntimeScope,
        agent_id: AgentId,
        unit: DispatchWorkUnit,
    ) -> Result<WithTaskPrelude> {
        let body = format_unit_ingress_body(&unit);
        if body.0.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "withTask records produced empty ingress body".into(),
            ));
        }
        let scope = dispatch_unit_runtime_scope(
            parent.context_id().clone(),
            agent_id,
            unit.unit_key.0.as_str(),
        );
        let prov_event = build_unit_user_message_event(&scope, unit.unit_key.0.as_str(), &body);
        self.store
            .add_event_with_logging(prov_event, "host ingress unit user message")
            .await;
        Ok(WithTaskPrelude {
            unit_key: unit.unit_key.0.clone(),
            scope,
            unit_history_ref: 1,
        })
    }

    async fn record_dispatch_accepted(
        &self,
        request: &AgentDispatchRequest,
        agent_package: &str,
        agent_instance: &str,
    ) -> Result<()> {
        let context_id = request
            .context_id
            .clone()
            .ok_or_else(|| BamlRtError::InvalidArgument("dispatch missing context_id".into()))?;
        let (source_kind, source_key) = dispatch_source_fields(request);
        let prov_event = ProvEvent::host_dispatch_accepted(
            context_id,
            request.routing_key.as_str().to_string(),
            request.message_type.as_str().to_string(),
            agent_package.to_string(),
            agent_instance.to_string(),
            source_kind,
            source_key,
        );
        self.store
            .add_event_with_logging(prov_event, "host dispatch accepted")
            .await;
        Ok(())
    }
}

fn dispatch_source_fields(request: &AgentDispatchRequest) -> (String, String) {
    if request.message_type.as_str() == wire::HOST_SOURCE_RECORDS_V1
        && let Some(batch) = request.messages.first()
        && let Some(kind) = batch.get("source_kind").and_then(|v| v.as_str())
        && let Some(key) = batch.get("source_key").and_then(|v| v.as_str())
    {
        return (kind.to_string(), key.to_string());
    }
    ("unknown".to_string(), "unknown".to_string())
}
