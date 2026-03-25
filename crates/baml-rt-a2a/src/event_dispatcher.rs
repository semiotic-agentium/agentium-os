//! In-process event dispatch pipeline.
//!
//! [`EventDispatcher`] polls registered [`EventProducer`]s, matches their events
//! against agent subscriptions, and delivers via [`AgentRegistry::handle_dispatch`].
//! Checkpoint cursors advance only after successful delivery — preserving
//! at-least-once semantics.

use std::sync::Arc;

use baml_rt_core::{
    AgentDiscoveryEntry, AgentInstanceId, AgentPackageName, AgentRouteKey, BamlRtError,
    EventDeliveryOutcome, ProducedEvent, Result,
    event_subscription::{PublishedEvent, subscriptions_match_published_event},
};
use baml_rt_tools::{EventProducer, ProducerRegistry};
use tracing::{info, warn};

use crate::AgentRegistry;

/// In-process event dispatcher: polls producers, matches subscribers, delivers
/// via [`AgentRegistry`].
pub struct EventDispatcher {
    registry: Arc<dyn AgentRegistry>,
    producers: ProducerRegistry,
}

impl EventDispatcher {
    pub fn new(registry: Arc<dyn AgentRegistry>) -> Self {
        Self {
            registry,
            producers: ProducerRegistry::new(),
        }
    }

    /// Register an event producer. Fails if source_kinds is empty or key is duplicate.
    pub fn register_producer(&mut self, producer: Arc<dyn EventProducer>) -> Result<()> {
        self.producers.register(producer)
    }

    /// Deliver a single [`ProducedEvent`] to all matched subscribers.
    ///
    /// Returns an error if no subscribers match (prevents silent event dropping).
    pub async fn deliver_event(&self, event: ProducedEvent) -> Result<EventDeliveryOutcome> {
        let published = event.as_published_event();
        let entries = self.registry.list_agents();
        let targets = matching_subscribers(&entries, &published);

        if targets.is_empty() {
            return Err(BamlRtError::InvalidArgument(format!(
                "no subscribed agents matched schema={schema}, source_kind={kind}, source_key={key}",
                schema = published.schema_version,
                kind = published.source_kind,
                key = published.source_key,
            )));
        }

        let request = event.into_dispatch_request();
        let mut outcome = EventDeliveryOutcome {
            subscribers_matched: targets.len(),
            subscribers_accepted: 0,
            failures: Vec::new(),
        };

        for target in &targets {
            match self.registry.handle_dispatch(target, request.clone()).await {
                Ok(ack) if ack.accepted => {
                    outcome.subscribers_accepted += 1;
                }
                Ok(ack) => {
                    let detail = ack.detail.unwrap_or_else(|| "rejected".into());
                    warn!(
                        agent_package = %target.agent_package,
                        agent_instance = %target.agent_instance_id,
                        detail = %detail,
                        "subscriber rejected dispatch"
                    );
                    outcome.failures.push((target.clone(), detail));
                }
                Err(err) => {
                    warn!(
                        agent_package = %target.agent_package,
                        agent_instance = %target.agent_instance_id,
                        error = %err,
                        "subscriber dispatch failed"
                    );
                    outcome.failures.push((target.clone(), err.to_string()));
                }
            }
        }

        Ok(outcome)
    }

    /// Poll all registered producers and deliver their events.
    ///
    /// Returns one `(producer_key, Result<EventDeliveryOutcome>)` per producer.
    ///
    /// - `Err` means the poll failed or no subscribers matched (hard failure).
    /// - `Ok(outcome)` with non-empty `outcome.failures` means some subscribers
    ///   rejected or errored (partial failure). The checkpoint is **not** advanced
    ///   in this case — only a fully clean delivery advances the cursor.
    pub async fn poll_and_deliver(
        &mut self,
    ) -> Vec<(
        String,
        std::result::Result<EventDeliveryOutcome, BamlRtError>,
    )> {
        let mut results = Vec::new();
        // Snapshot producer list so we can mutate checkpoints after.
        let producers: Vec<Arc<dyn EventProducer>> = self.producers.producers().to_vec();

        for producer in &producers {
            let key = producer.producer_key().to_string();
            let checkpoint = self.producers.checkpoint(&key);

            let poll_result = match producer.poll(&checkpoint).await {
                Ok(poll) => poll,
                Err(err) => {
                    warn!(producer_key = %key, error = %err, "producer poll failed");
                    results.push((key, Err(err)));
                    continue;
                }
            };

            if poll_result.events.is_empty() {
                info!(producer_key = %key, "producer poll returned no events");
                results.push((
                    key,
                    Ok(EventDeliveryOutcome {
                        subscribers_matched: 0,
                        subscribers_accepted: 0,
                        failures: Vec::new(),
                    }),
                ));
                continue;
            }

            let mut aggregate = EventDeliveryOutcome {
                subscribers_matched: 0,
                subscribers_accepted: 0,
                failures: Vec::new(),
            };
            let mut short_circuit_err: Option<BamlRtError> = None;

            for event in poll_result.events {
                match self.deliver_event(event).await {
                    Ok(outcome) => {
                        aggregate.subscribers_matched += outcome.subscribers_matched;
                        aggregate.subscribers_accepted += outcome.subscribers_accepted;
                        aggregate.failures.extend(outcome.failures);
                    }
                    Err(err) => {
                        warn!(
                            producer_key = %key,
                            error = %err,
                            "event delivery failed"
                        );
                        short_circuit_err = Some(err);
                        break;
                    }
                }
            }

            if let Some(err) = short_circuit_err {
                results.push((key, Err(err)));
            } else {
                // Only advance cursor if every event was delivered without failures.
                if aggregate.failures.is_empty() {
                    self.producers
                        .advance_checkpoint(&key, poll_result.checkpoint);
                }
                results.push((key, Ok(aggregate)));
            }
        }

        results
    }
}

/// Find agents whose subscriptions match a published event.
fn matching_subscribers(
    entries: &[AgentDiscoveryEntry],
    event: &PublishedEvent,
) -> Vec<AgentRouteKey> {
    entries
        .iter()
        .filter(|entry| subscriptions_match_published_event(&entry.agent_card.subscriptions, event))
        .filter_map(|entry| {
            let package = AgentPackageName::parse(&entry.agent_package)?;
            let instance = AgentInstanceId::parse(&entry.agent_instance_id)?;
            Some(AgentRouteKey::new(package, instance))
        })
        .collect()
}
