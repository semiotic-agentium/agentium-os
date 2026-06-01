// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Surreal-backed [`HostIngressRecorder`] for publish and dispatch `withTask` preludes.

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::{
    AgentDispatchRequest, BamlRtError, DispatchTarget, HostIngressRecorder,
    IngressPollUserMessageRef, ProducedEvent, Result,
    context::RuntimeScope,
    dispatch_ingress::{
        DispatchWorkUnit, WithTaskPrelude, dispatch_unit_runtime_scope, format_unit_ingress_body,
    },
    host_source_records_body::IngressPollBody,
    ids::{AgentId, MessageId, UuidId},
};
use baml_rt_vocabulary::vocabulary::user_speaker_kinds;
use uuid::Uuid;

use crate::{
    ProvEvent, ProvenanceWriter, SurrealProvenanceStore,
    events::ProvEventData,
    host_ingress_identity::{
        activity_anchor_for_ingress_poll_user, activity_anchor_for_ingress_unit_user,
    },
    host_ingress_types::{HostDispatchFailureKind, HostDispatchRejectedSpec, HostIngressSourceRef},
    task_agent_binding::{TaskAgentBinding, TaskAgentBindingSource},
};

/// Writes host ingress poll/unit user messages and lineage events to Surreal provenance.
pub struct HostIngressRecorderImpl {
    store: Arc<SurrealProvenanceStore>,
}

impl HostIngressRecorderImpl {
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

fn host_poll_ingress_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::nil()))
}

fn ingress_user_message_metadata() -> Option<HashMap<String, String>> {
    Some(HashMap::from([(
        "user_speaker_kind".to_string(),
        user_speaker_kinds::INGRESS.to_string(),
    )]))
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
    let anchor = activity_anchor_for_ingress_poll_user(&context_id, batch_message_id);
    let agent_id = host_poll_ingress_agent_id();
    let content = vec![body.0.clone()];
    let timestamp_ms =
        baml_rt_core::now_unix_ms(baml_rt_core::clock_events::HOST_INGRESS_POLL_USER);
    if let Some(task_id) = event.task_id.clone() {
        return Ok(ProvEvent::Task(crate::events::TaskScopedEvent {
            id: anchor,
            context_id,
            task_id,
            timestamp_ms,
            data: ProvEventData::MessageReceived {
                id: message_id,
                role: "user".to_string(),
                content,
                metadata: ingress_user_message_metadata(),
                agent_id: host_poll_ingress_agent_id(),
                citations: Vec::new(),
            },
        }));
    }
    Ok(ProvEvent::Global(crate::events::GlobalEvent {
        id: anchor,
        context_id,
        timestamp_ms,
        data: ProvEventData::MessageReceived {
            id: message_id.clone(),
            role: "user".to_string(),
            content,
            metadata: ingress_user_message_metadata(),
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
    let anchor = activity_anchor_for_ingress_unit_user(&context_id, unit_key);
    ProvEvent::Task(crate::events::TaskScopedEvent {
        id: anchor,
        context_id,
        task_id,
        timestamp_ms: baml_rt_core::now_unix_ms(baml_rt_core::clock_events::HOST_INGRESS_UNIT_USER),
        data: ProvEventData::MessageReceived {
            id: message_id,
            role: "user".to_string(),
            content: vec![body.0.clone()],
            metadata: ingress_user_message_metadata(),
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
        let source_kind = event
            .messages
            .first()
            .and_then(|m| m.get("source_kind"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let source_key = event
            .messages
            .first()
            .and_then(|m| m.get("source_key"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let source_cursor = event
            .messages
            .first()
            .and_then(|m| m.get("source_cursor"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let record_count = event.messages.len();
        let source_message_ts: Vec<String> = event
            .messages
            .iter()
            .filter_map(|m| m.get("message_ts").and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect();
        let prov_event = ProvEvent::host_source_poll_recorded(
            context_id,
            source_kind,
            source_key,
            source_cursor,
            event.schema_version.as_str().to_string(),
            record_count,
            source_message_ts,
        );
        self.store
            .add_event_with_logging(prov_event, "host source poll recorded")
            .await;
        Ok(())
    }

    async fn record_ingress_poll_user_message(
        &self,
        event: &ProducedEvent,
        body: &IngressPollBody,
    ) -> Result<IngressPollUserMessageRef> {
        let batch_message_id = event.message_id.as_deref().ok_or_else(|| {
            BamlRtError::InvalidArgument("ProducedEvent missing message_id".into())
        })?;
        let prov_event = build_poll_user_message_event(event, body)?;
        self.store
            .add_event_with_logging(prov_event, "host ingress poll user message")
            .await;
        Ok(IngressPollUserMessageRef {
            message_id: MessageId::from(batch_message_id),
        })
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
        let task_id = scope
            .task_id_opt()
            .expect("dispatch unit scope must be task-scoped")
            .clone();
        let context_id = scope.context_id().clone();
        let binding = TaskAgentBinding::new(
            context_id,
            task_id,
            scope.agent_id().clone(),
            TaskAgentBindingSource::HostWithTaskPrelude,
        )
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?;
        self.store
            .bind_task_executing_agent(binding)
            .await
            .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?;
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
        target: DispatchTarget,
    ) -> Result<()> {
        let context_id = request
            .context_id
            .clone()
            .ok_or_else(|| BamlRtError::InvalidArgument("dispatch missing context_id".into()))?;
        let source = HostIngressSourceRef::from_dispatch_request(request);
        let (source_kind, source_key) = match &source {
            HostIngressSourceRef::SourceRecords { kind, key } => (kind.clone(), key.clone()),
            HostIngressSourceRef::Unspecified => (
                HostIngressSourceRef::UNSPECIFIED_KIND.to_string(),
                HostIngressSourceRef::UNSPECIFIED_KEY.to_string(),
            ),
        };
        let prov_event = ProvEvent::host_dispatch_accepted(
            context_id,
            request.routing_key.as_str().to_string(),
            request.message_type.as_str().to_string(),
            target,
            source_kind,
            source_key,
        );
        self.store
            .add_event_with_logging(prov_event, "host dispatch accepted")
            .await;
        Ok(())
    }

    async fn record_dispatch_rejected(
        &self,
        request: &AgentDispatchRequest,
        target: DispatchTarget,
        detail: &str,
        transport_failure: bool,
    ) -> Result<()> {
        let context_id = request
            .context_id
            .clone()
            .ok_or_else(|| BamlRtError::InvalidArgument("dispatch missing context_id".into()))?;
        let prov_event = ProvEvent::host_dispatch_rejected(HostDispatchRejectedSpec {
            context_id,
            routing_key: request.routing_key.as_str().to_string(),
            schema_version: request.message_type.clone(),
            target,
            source: HostIngressSourceRef::from_dispatch_request(request),
            detail: detail.to_string(),
            failure_kind: HostDispatchFailureKind::from_transport_flag(transport_failure),
        });
        self.store
            .add_event_with_logging(prov_event, "host dispatch rejected")
            .await;
        Ok(())
    }
}
