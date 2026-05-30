// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    AgentDispatchRoutingKey, BamlRtError, EventSchemaVersion, EventSourceKind, ProducedEvent,
    Result, clock_events, event_subscription::EventSourceKey, now_unix_ms,
};
use baml_rt_tools::{
    EventProducer, EventProducerBuildContext, EventProducerBuildFuture, EventProducerProvider,
    ProducerCheckpoint, ProducerPoll,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, warn};

use crate::{
    callback_delivery_gate::callback_delivery_gate,
    callback_store::{StoredCallback, callback_store},
};

pub const CALLBACK_EVENT_SCHEMA_VERSION: &str = "system.callback.v1";
pub const CALLBACK_EVENT_ROUTING_KEY: &str = "system:callback";
pub const CALLBACK_SOURCE_KIND: &str = "system/callback";

/// Re-export: canonical keys on [`AgentDispatchRequest::metadata`](baml_rt_core::AgentDispatchRequest).
pub use baml_rt_core::{
    DISPATCH_METADATA_SCHEDULING_CONTEXT_ID, DISPATCH_METADATA_SCHEDULING_TASK_ID,
};

const MAX_CALLBACKS_PER_POLL: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CallbackProducerCheckpoint {
    #[serde(default)]
    delivered_callback_ids: Vec<String>,
}

impl CallbackProducerCheckpoint {
    fn from_checkpoint(checkpoint: &ProducerCheckpoint) -> Self {
        match checkpoint.value() {
            Some(raw) => serde_json::from_str(raw).unwrap_or_else(|err| {
                warn!(
                    error = %err,
                    "corrupt system/callback checkpoint; resetting delivery reconciliation state"
                );
                Self::default()
            }),
            None => Self::default(),
        }
    }

    fn to_checkpoint(&self) -> ProducerCheckpoint {
        match serde_json::to_string(self) {
            Ok(value) => ProducerCheckpoint::some(value),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "failed to serialize system/callback checkpoint; cursor will not advance"
                );
                ProducerCheckpoint::none()
            }
        }
    }
}

pub struct CallbackEventProducer {
    producer_key: &'static str,
    routing_key: AgentDispatchRoutingKey,
    schema_version: EventSchemaVersion,
    source_kind: EventSourceKind,
    source_kinds: Vec<EventSourceKind>,
}

impl CallbackEventProducer {
    fn new() -> Result<Self> {
        let routing_key =
            AgentDispatchRoutingKey::parse(CALLBACK_EVENT_ROUTING_KEY).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "invalid static callback routing key '{routing_key}'",
                    routing_key = CALLBACK_EVENT_ROUTING_KEY
                ))
            })?;
        let schema_version =
            EventSchemaVersion::parse(CALLBACK_EVENT_SCHEMA_VERSION).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "invalid static callback schema version '{schema_version}'",
                    schema_version = CALLBACK_EVENT_SCHEMA_VERSION
                ))
            })?;
        let source_kind = EventSourceKind::parse(CALLBACK_SOURCE_KIND).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "invalid static callback source kind '{source_kind}'",
                source_kind = CALLBACK_SOURCE_KIND
            ))
        })?;
        Ok(Self {
            producer_key: CALLBACK_SOURCE_KIND,
            routing_key,
            schema_version,
            source_kinds: vec![source_kind.clone()],
            source_kind,
        })
    }
}

#[async_trait]
impl EventProducer for CallbackEventProducer {
    fn producer_key(&self) -> &str {
        self.producer_key
    }

    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.source_kinds
    }

    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        let store = callback_store().ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "system/callback producer requires an installed callback store".to_string(),
            )
        })?;
        let checkpoint_state = CallbackProducerCheckpoint::from_checkpoint(checkpoint);
        let reconciled_deliveries = !checkpoint_state.delivered_callback_ids.is_empty();
        if !checkpoint_state.delivered_callback_ids.is_empty() {
            store
                .mark_callbacks_delivered(
                    &checkpoint_state.delivered_callback_ids,
                    now_unix_ms(clock_events::SYSTEM_CALLBACK_CHECKPOINT_RECONCILE),
                )
                .await?;
            debug!(
                delivered_count = checkpoint_state.delivered_callback_ids.len(),
                "system/callback reconciled delivered callbacks from persisted checkpoint"
            );
        }

        let due_callbacks = store
            .list_due_callbacks(
                now_unix_ms(clock_events::SYSTEM_CALLBACK_DUE_POLL),
                MAX_CALLBACKS_PER_POLL,
            )
            .await?;
        let due_callbacks: Vec<StoredCallback> = due_callbacks
            .into_iter()
            .filter(|callback| {
                if callback.scheduling_context_id.is_none()
                    || callback.scheduling_task_id.is_none()
                {
                    warn!(
                        callback_id = %callback.callback_id,
                        source_key = %callback.source_key,
                        "skipping scheduled callback without scheduling scope (pre-cutover pending row or corrupt); not emitted"
                    );
                    false
                } else {
                    true
                }
            })
            .collect();
        let due_callbacks = if let Some(gate) = callback_delivery_gate() {
            let mut deliverable = Vec::with_capacity(due_callbacks.len());
            let mut deferred_count = 0usize;
            for callback in due_callbacks {
                if gate.can_emit_callback(&callback).await? {
                    deliverable.push(callback);
                } else {
                    deferred_count += 1;
                }
            }
            if deferred_count > 0 {
                debug!(
                    deferred_count,
                    deliverable_count = deliverable.len(),
                    "system/callback deferred due callbacks until host delivery gate opens"
                );
            }
            deliverable
        } else {
            due_callbacks
        };
        let due_callback_ids = due_callbacks
            .iter()
            .map(|callback| callback.callback_id.clone())
            .collect::<Vec<_>>();
        let emitted_callback_ids = store
            .mark_callbacks_emitted(
                &due_callback_ids,
                now_unix_ms(clock_events::SYSTEM_CALLBACK_MARK_EMITTED),
            )
            .await?;
        let emitted_callback_id_set: HashSet<&str> =
            emitted_callback_ids.iter().map(String::as_str).collect();
        let due_callbacks = due_callbacks
            .into_iter()
            .filter(|callback| emitted_callback_id_set.contains(callback.callback_id.as_str()))
            .collect::<Vec<_>>();
        if emitted_callback_ids.len() < due_callback_ids.len() {
            debug!(
                requested_count = due_callback_ids.len(),
                emitted_count = emitted_callback_ids.len(),
                "system/callback skipped rows that were no longer pending during emission claim"
            );
        }

        let delivered_callback_ids = due_callbacks
            .iter()
            .map(|callback| callback.callback_id.clone())
            .collect::<Vec<_>>();
        let events = due_callbacks
            .into_iter()
            .map(|callback| self.callback_to_event(callback))
            .collect::<Result<Vec<_>>>()?;
        let checkpoint = if events.is_empty() && delivered_callback_ids.is_empty() {
            if reconciled_deliveries {
                CallbackProducerCheckpoint::default().to_checkpoint()
            } else {
                ProducerCheckpoint::none()
            }
        } else {
            CallbackProducerCheckpoint {
                delivered_callback_ids,
            }
            .to_checkpoint()
        };

        if !events.is_empty() {
            debug!(
                event_count = events.len(),
                "system/callback produced due callback events"
            );
        } else if reconciled_deliveries {
            debug!("system/callback cleared reconciled delivery checkpoint");
        }

        Ok(ProducerPoll { events, checkpoint })
    }
}

impl CallbackEventProducer {
    fn callback_to_event(&self, callback: StoredCallback) -> Result<ProducedEvent> {
        let callback_source_key = callback.source_key.clone();
        let source_key = EventSourceKey::parse(&callback_source_key).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "system/callback stored invalid source key '{source_key}'",
                source_key = callback_source_key
            ))
        })?;
        let callback_id = callback.callback_id.clone();

        let scheduling_context_str = callback
            .scheduling_context_id
            .as_ref()
            .map(|id| id.as_str().to_string());
        let scheduling_task_str = callback
            .scheduling_task_id
            .as_ref()
            .map(|id| id.as_str().to_string());
        let dispatch_metadata = match (&scheduling_context_str, &scheduling_task_str) {
            (Some(sc), Some(st)) => Some(json!({
                DISPATCH_METADATA_SCHEDULING_CONTEXT_ID: sc,
                DISPATCH_METADATA_SCHEDULING_TASK_ID: st,
            })),
            _ => None,
        };

        tracing::info!(
            callback_id = %callback_id,
            dispatch_context = ?callback.context_id.as_ref().map(|c| c.as_str()),
            dispatch_task = ?callback.task_id.as_ref().map(|t| t.as_str()),
            scheduling_context = ?callback.scheduling_context_id.as_ref().map(|c| c.as_str()),
            scheduling_task = ?callback.scheduling_task_id.as_ref().map(|t| t.as_str()),
            "system/callback producer emitting due callback dispatch envelope"
        );

        Ok(ProducedEvent {
            routing_key: self.routing_key.clone(),
            schema_version: self.schema_version.clone(),
            source_kind: self.source_kind.clone(),
            source_key,
            messages: vec![json!({
                "schema_version": CALLBACK_EVENT_SCHEMA_VERSION,
                "callback_id": callback.callback_id,
                "source": {
                    "source_kind": CALLBACK_SOURCE_KIND,
                    "source_key": callback.source_key,
                },
                "scheduled_for_unix_ms": callback.scheduled_for_unix_ms,
                "requested_at_unix_ms": callback.requested_at_unix_ms,
                "emitted_at_unix_ms": now_unix_ms(clock_events::SYSTEM_CALLBACK_EMIT),
                "dedupe_key": callback.dedupe_key,
                "payload": callback.payload,
                "request": {
                    "context_id": callback.context_id.as_ref().map(|id| id.as_str()),
                    "task_id": callback.task_id.as_ref().map(|id| id.as_str()),
                    "scheduling_context_id": scheduling_context_str.as_deref(),
                    "scheduling_task_id": scheduling_task_str.as_deref(),
                    "requesting_agent_id": callback.requesting_agent_id.as_deref(),
                    "requesting_message_id": callback
                        .requesting_message_id
                        .as_ref()
                        .map(|id| id.as_str()),
                },
            })],
            context_id: callback.context_id,
            task_id: callback.task_id,
            message_id: Some(format!(
                "system/callback:{callback_id}",
                callback_id = callback_id
            )),
            metadata: dispatch_metadata,
        })
    }
}

pub fn build_callback_event_producers(_ctx: EventProducerBuildContext) -> EventProducerBuildFuture {
    Box::pin(async move {
        if callback_store().is_none() {
            warn!(
                "system/callback store is not installed; skipping callback producer registration"
            );
            return Ok(vec![]);
        }
        Ok(vec![
            Arc::new(CallbackEventProducer::new()?) as Arc<dyn EventProducer>
        ])
    })
}

inventory::submit! {
    EventProducerProvider {
        tool_name: "system/callback",
        build: build_callback_event_producers,
    }
}
