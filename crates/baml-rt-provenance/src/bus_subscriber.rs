// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Bus subscriber: maps command/event/effect envelopes to provenance events.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::bus::{EffectSubscriber, Envelope, Payload, Subscriber};

use crate::{
    effect_subscriber::ProvenanceEffectSubscriber, events::ProvEvent, store::ProvenanceWriter,
};

pub struct ProvenanceBusSubscriber {
    writer: Arc<dyn ProvenanceWriter>,
    effect_delegate: ProvenanceEffectSubscriber,
}

impl ProvenanceBusSubscriber {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self {
            effect_delegate: ProvenanceEffectSubscriber::new(writer.clone()),
            writer,
        }
    }
}

#[async_trait]
impl Subscriber for ProvenanceBusSubscriber {
    async fn on_envelope(&self, envelope: &Envelope) -> baml_rt_core::Result<()> {
        match &envelope.payload {
            Payload::Effect(effect) => {
                self.effect_delegate.on_effect(effect).await?;
            }
            Payload::Command(command) => {
                if let Some(scope) = &envelope.scope {
                    let event = ProvEvent::message_received_global(
                        scope.context_id().clone(),
                        scope.message_id().clone(),
                        "ROLE_USER".to_string(),
                        vec![format!("command: {}", command.name)],
                        None,
                        scope.agent_id().clone(),
                        envelope.timestamp_ms,
                    );
                    self.writer
                        .add_event_with_logging(event, "bus subscriber command")
                        .await;
                }
            }
            Payload::DomainEvent(event) => {
                if let Some(scope) = &envelope.scope {
                    let event = ProvEvent::message_sent_global(
                        scope.context_id().clone(),
                        scope.message_id().clone(),
                        "ROLE_AGENT".to_string(),
                        vec![format!("event: {}", event.name)],
                        None,
                        scope.agent_id().clone(),
                        envelope.timestamp_ms,
                        Vec::new(),
                    );
                    self.writer
                        .add_event_with_logging(event, "bus subscriber event")
                        .await;
                }
            }
        }
        Ok(())
    }
}
