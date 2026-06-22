// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host-managed Grafana alert event producer.
//!
//! Drains the shared [`IngressStore`] for items enqueued by the webhook
//! intake (`crate::webhook::enqueue_webhook`) and emits `grafana.alert.v1`
//! [`ProducedEvent`]s with the resolved `context_id` and stable `message_id`.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    AgentDispatchRoutingKey, BamlRtError, EventSchemaVersion, EventSourceKind, IngressStore,
    ProducedEvent, Result, clock_events,
    ingress_store::{IngressId, IngressItem},
};
use baml_rt_tools::{
    EventProducer, EventProducerBuildContext, EventProducerBuildFuture, EventProducerProvider,
    ProducerCheckpoint, ProducerPoll,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::webhook::GrafanaIngressEnvelope;

pub const GRAFANA_ALERT_SCHEMA_VERSION: &str = "grafana.alert.v1";
pub const GRAFANA_ROUTING_KEY: &str = "grafana:intake";
pub const GRAFANA_SOURCE_KIND: &str = "grafana";
pub const GRAFANA_INBOX_PRODUCER_KEY: &str = "support/grafana-alerts:inbox";

const MAX_ITEMS_PER_POLL: usize = 100;
const RETRY_AFTER_MS: u64 = 60_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InboxCheckpoint {
    #[serde(default)]
    delivered_ingress_ids: Vec<IngressId>,
}

impl InboxCheckpoint {
    fn from_checkpoint(checkpoint: &ProducerCheckpoint) -> Self {
        match checkpoint.value() {
            Some(raw) => serde_json::from_str(raw).unwrap_or_else(|err| {
                warn!(
                    error = %err,
                    "corrupt grafana-alerts inbox checkpoint; resetting reconciliation state"
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
                    "failed to serialize grafana-alerts inbox checkpoint; cursor will not advance"
                );
                ProducerCheckpoint::none()
            }
        }
    }
}

pub struct GrafanaAlertEventProducer {
    store: Arc<dyn IngressStore>,
    producer_key: &'static str,
    routing_key: AgentDispatchRoutingKey,
    schema_version: EventSchemaVersion,
    source_kinds: Vec<EventSourceKind>,
}

impl GrafanaAlertEventProducer {
    pub fn new(store: Arc<dyn IngressStore>) -> Result<Self> {
        let routing_key = AgentDispatchRoutingKey::parse(GRAFANA_ROUTING_KEY).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "invalid Grafana routing key '{GRAFANA_ROUTING_KEY}'"
            ))
        })?;
        let schema_version =
            EventSchemaVersion::parse(GRAFANA_ALERT_SCHEMA_VERSION).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "invalid Grafana schema version '{GRAFANA_ALERT_SCHEMA_VERSION}'"
                ))
            })?;
        let source_kind = EventSourceKind::parse(GRAFANA_SOURCE_KIND).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "invalid Grafana source kind '{GRAFANA_SOURCE_KIND}'"
            ))
        })?;
        Ok(Self {
            store,
            producer_key: GRAFANA_INBOX_PRODUCER_KEY,
            routing_key,
            schema_version,
            source_kinds: vec![source_kind],
        })
    }

    fn ingress_item_to_event(&self, item: &IngressItem) -> Result<ProducedEvent> {
        let envelope: GrafanaIngressEnvelope =
            serde_json::from_str(&item.payload_json).map_err(|err| {
                BamlRtError::InvalidArgument(format!(
                    "grafana-alerts ingress payload deserialize failed: {err}"
                ))
            })?;
        let context_id = baml_rt_core::ContextId::from(envelope.context_id.as_str());
        let message_payload = serde_json::to_value(&envelope.alert).map_err(|err| {
            BamlRtError::InvalidArgument(format!(
                "grafana-alerts failed to serialize alert payload: {err}"
            ))
        })?;
        Ok(ProducedEvent {
            routing_key: self.routing_key.clone(),
            schema_version: self.schema_version.clone(),
            source_kind: self.source_kinds[0].clone(),
            source_key: item.source_key.clone(),
            messages: vec![message_payload],
            context_id: Some(context_id),
            task_id: None,
            message_id: Some(envelope.message_id),
            producer_key: None,
            metadata: None,
        })
    }
}

#[async_trait]
impl EventProducer for GrafanaAlertEventProducer {
    fn producer_key(&self) -> &str {
        self.producer_key
    }

    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.source_kinds
    }

    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        let store = self.store.as_ref();
        let now_ms = baml_rt_core::now_unix_ms(clock_events::GRAFANA_INGRESS);
        let checkpoint_state = InboxCheckpoint::from_checkpoint(checkpoint);
        let reconciled = !checkpoint_state.delivered_ingress_ids.is_empty();
        if reconciled {
            store
                .mark_delivered(&checkpoint_state.delivered_ingress_ids, now_ms)
                .await?;
            debug!(
                delivered_count = checkpoint_state.delivered_ingress_ids.len(),
                "grafana-alerts inbox reconciled delivered ingress items"
            );
        }

        let reclaimed = store
            .requeue_stale(now_ms.saturating_sub(RETRY_AFTER_MS))
            .await?;
        if reclaimed > 0 {
            warn!(
                reclaimed_count = reclaimed,
                retry_after_ms = RETRY_AFTER_MS,
                "grafana-alerts inbox reclaimed stale emitted items after delivery timeout"
            );
        }

        let pending = store
            .list_pending(&self.source_kinds, MAX_ITEMS_PER_POLL)
            .await?;
        let pending_ids = pending
            .iter()
            .map(|item| item.ingress_id.clone())
            .collect::<Vec<_>>();
        let emitted_ids = store.mark_emitted(&pending_ids, now_ms).await?;
        let emitted_set = emitted_ids
            .iter()
            .map(IngressId::as_str)
            .collect::<HashSet<_>>();
        let claimed = pending
            .into_iter()
            .filter(|item| emitted_set.contains(item.ingress_id.as_str()))
            .collect::<Vec<_>>();

        let mut events = Vec::with_capacity(claimed.len());
        let mut delivered_ingress_ids = Vec::with_capacity(claimed.len());
        for item in &claimed {
            match self.ingress_item_to_event(item) {
                Ok(event) => {
                    delivered_ingress_ids.push(item.ingress_id.clone());
                    events.push(event);
                }
                Err(err) => {
                    warn!(
                        ingress_id = %item.ingress_id,
                        error = %err,
                        "grafana-alerts dropping malformed ingress item"
                    );
                    store
                        .mark_delivered(std::slice::from_ref(&item.ingress_id), now_ms)
                        .await?;
                }
            }
        }

        let checkpoint = if events.is_empty() && delivered_ingress_ids.is_empty() {
            if reconciled {
                InboxCheckpoint::default().to_checkpoint()
            } else {
                ProducerCheckpoint::none()
            }
        } else {
            InboxCheckpoint {
                delivered_ingress_ids,
            }
            .to_checkpoint()
        };

        Ok(ProducerPoll { events, checkpoint })
    }
}

pub fn build_grafana_alert_event_producers(
    ctx: EventProducerBuildContext,
) -> EventProducerBuildFuture {
    Box::pin(async move {
        let Some(store) = ctx.ingress_store else {
            return Ok(Vec::new());
        };
        let producer: Arc<dyn EventProducer> = Arc::new(GrafanaAlertEventProducer::new(store)?);
        Ok(vec![producer])
    })
}

inventory::submit! {
    EventProducerProvider {
        tool_name: "support/grafana-alerts",
        build: build_grafana_alert_event_producers,
    }
}
