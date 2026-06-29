// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! In-process event dispatch pipeline.
//!
//! [`EventDispatcher`] polls registered [`EventProducer`]s, matches their events
//! against agent subscriptions, and delivers via [`AgentRegistry::handle_dispatch`].
//! Checkpoint cursors advance only after a poll cycle reaches the configured
//! success boundary:
//! - subscriber delivery failures remain sticky and keep the checkpoint pinned
//! - events with no matching subscribers are logged and treated as handled so
//!   the host does not stall an entire source on permanently unconsumed data

use std::{sync::Arc, time::Instant};

use baml_rt_core::{BamlRtError, EventDeliveryOutcome, ProducedEvent, Result};
use baml_rt_observability::metrics;
use baml_rt_tools::{EventProducer, ProducerCheckpoint, ProducerRegistry};
use baml_tools_system::callback_producer::CALLBACK_SOURCE_KIND;
use tracing::{debug, warn};

use crate::{AgentRegistry, RegistryDispatchPort};

/// In-process event dispatcher: polls producers, matches subscribers, delivers
/// via [`AgentRegistry`].
pub struct EventDispatcher {
    registry: Arc<dyn AgentRegistry>,
    producers: ProducerRegistry,
    publish_service: Arc<baml_rt_core::HostPublishService>,
}

impl EventDispatcher {
    pub fn new(
        registry: Arc<dyn AgentRegistry>,
        publish_service: Arc<baml_rt_core::HostPublishService>,
    ) -> Self {
        Self {
            registry,
            producers: ProducerRegistry::new(),
            publish_service,
        }
    }

    /// Register an event producer. Fails if source_kinds is empty or key is duplicate.
    pub fn register_producer(&mut self, producer: Arc<dyn EventProducer>) -> Result<()> {
        self.producers.register(producer)
    }

    /// Register an event producer with a preloaded checkpoint.
    pub fn register_producer_with_checkpoint(
        &mut self,
        producer: Arc<dyn EventProducer>,
        checkpoint: ProducerCheckpoint,
    ) -> Result<()> {
        self.producers
            .register_with_checkpoint(producer, checkpoint)
    }

    /// Current checkpoint for a registered producer.
    pub fn checkpoint(&self, producer_key: &str) -> ProducerCheckpoint {
        self.producers.checkpoint(producer_key)
    }

    /// Deliver a single [`ProducedEvent`] to all matched subscribers.
    ///
    /// Returns a zero-match outcome when no subscribers match.
    pub async fn deliver_event(&self, event: ProducedEvent) -> Result<EventDeliveryOutcome> {
        self.deliver_event_with_producer_key(event, None).await
    }

    /// Same as [`Self::deliver_event`], optionally attributing OTLP metrics to a bounded `producer_key`.
    pub async fn deliver_event_with_producer_key(
        &self,
        mut event: ProducedEvent,
        producer_key: Option<&str>,
    ) -> Result<EventDeliveryOutcome> {
        if event.producer_key.is_none() {
            event.producer_key = producer_key.map(ToOwned::to_owned);
        }
        let published = event.as_published_event();
        let entries = self.registry.list_agents();
        let port = RegistryDispatchPort::new(self.registry.as_ref());
        let outcome = self.publish_service.publish(&entries, event, &port).await?;
        if outcome.subscribers_matched == 0 {
            warn!(
                schema = %published.schema_version,
                source_kind = %published.source_kind,
                source_key = %published.source_key,
                "no subscribed agents matched produced event; advancing checkpoint"
            );
            metrics::record_event_dispatch_no_subscribers(producer_key.unwrap_or("unknown"));
            return Ok(outcome);
        }
        for (target, failure) in &outcome.failures {
            warn!(
                agent_package = %target.agent_package,
                agent_instance = %target.agent_instance_id,
                detail = %failure.detail(),
                "subscriber dispatch failed or rejected"
            );
        }

        if let Some(pk) = producer_key {
            let outcome_label = if outcome.failures.is_empty() {
                if outcome.subscribers_accepted == outcome.subscribers_matched {
                    "all_accepted"
                } else {
                    "partial_rejection"
                }
            } else {
                "partial_rejection"
            };
            metrics::record_event_dispatch_subscriber_batch(
                pk,
                outcome.subscribers_matched,
                outcome_label,
            );
        }

        Ok(outcome)
    }

    /// Poll all registered producers and deliver their events.
    ///
    /// Returns one `(producer_key, Result<EventDeliveryOutcome>)` per producer.
    ///
    /// - `Err` means the poll failed or delivery hit a hard failure.
    /// - `Ok(outcome)` with non-empty `outcome.failures` means some subscribers
    ///   rejected or errored (partial failure). The checkpoint is **not** advanced
    ///   in this case — only a fully clean delivery advances the cursor.
    pub async fn poll_and_deliver(
        &mut self,
    ) -> Vec<(
        String,
        std::result::Result<EventDeliveryOutcome, BamlRtError>,
    )> {
        let cycle_start = Instant::now();
        let mut results = Vec::new();
        // Snapshot producer list so we can mutate checkpoints after.
        let producers: Vec<Arc<dyn EventProducer>> = self.producers.producers().to_vec();

        for producer in &producers {
            let key = producer.producer_key().to_string();
            let checkpoint = self.producers.checkpoint(&key);
            let producer_start = Instant::now();

            let poll_result = match producer.poll(&checkpoint).await {
                Ok(poll) => poll,
                Err(err) => {
                    warn!(producer_key = %key, error = %err, "producer poll failed");
                    metrics::record_event_poll_producer(
                        &key,
                        "poll_error",
                        producer_start.elapsed(),
                        0,
                    );
                    results.push((key, Err(err)));
                    continue;
                }
            };

            if poll_result.events.is_empty() {
                self.producers
                    .advance_checkpoint(&key, poll_result.checkpoint.clone());
                debug!(producer_key = %key, "producer poll returned no events");
                metrics::record_event_poll_producer(&key, "empty", producer_start.elapsed(), 0);
                results.push((
                    key,
                    Ok(EventDeliveryOutcome {
                        subscribers_matched: 0,
                        subscribers_accepted: 0,
                        acceptances: Vec::new(),
                        failures: Vec::new(),
                    }),
                ));
                continue;
            }

            let batch_len = poll_result.events.len() as u64;
            let baml_rt_tools::ProducerPoll {
                events,
                checkpoint: poll_checkpoint,
            } = poll_result;

            let mut aggregate = EventDeliveryOutcome {
                subscribers_matched: 0,
                subscribers_accepted: 0,
                acceptances: Vec::new(),
                failures: Vec::new(),
            };
            let mut short_circuit: Option<(BamlRtError, &'static str)> = None;
            // `CallbackEventProducer` checkpoint payload reconciles rows as delivered on the next
            // poll. Advancing after a zero-subscriber "delivery" would mark callbacks delivered
            // without `handle_dispatch` ever running.
            let system_callback_producer_key = CALLBACK_SOURCE_KIND;
            let mut system_callback_blocked_on_zero_subscribers = false;

            let declared_kinds = producer.source_kinds();
            for event in events {
                if !declared_kinds.contains(&event.source_kind) {
                    let err = BamlRtError::InvalidArgument(format!(
                        "producer {key} declared source_kinds {declared:?} but emitted \
                         event with source_kind={actual}",
                        declared = declared_kinds
                            .iter()
                            .map(|k| k.as_str())
                            .collect::<Vec<_>>(),
                        actual = event.source_kind,
                    ));
                    warn!(
                        producer_key = %key,
                        error = %err,
                        "source kind mismatch"
                    );
                    short_circuit = Some((err, "validation_error"));
                    break;
                }
                match self
                    .deliver_event_with_producer_key(event, Some(key.as_str()))
                    .await
                {
                    Ok(outcome) => {
                        if key == system_callback_producer_key && outcome.subscribers_matched == 0 {
                            system_callback_blocked_on_zero_subscribers = true;
                        }
                        aggregate.subscribers_matched += outcome.subscribers_matched;
                        aggregate.subscribers_accepted += outcome.subscribers_accepted;
                        aggregate.acceptances.extend(outcome.acceptances);
                        aggregate.failures.extend(outcome.failures);
                    }
                    Err(err) => {
                        warn!(
                            producer_key = %key,
                            error = %err,
                            "event delivery failed"
                        );
                        short_circuit = Some((err, "delivery_error"));
                        break;
                    }
                }
            }

            if let Some((err, kind)) = short_circuit {
                metrics::record_event_poll_producer(&key, kind, producer_start.elapsed(), 0);
                results.push((key, Err(err)));
            } else {
                // Only advance cursor if every event was delivered without failures.
                if aggregate.failures.is_empty() && !system_callback_blocked_on_zero_subscribers {
                    self.producers.advance_checkpoint(&key, poll_checkpoint);
                    metrics::record_event_poll_producer(
                        &key,
                        "success",
                        producer_start.elapsed(),
                        batch_len,
                    );
                } else if !aggregate.failures.is_empty() {
                    metrics::record_event_poll_producer(
                        &key,
                        "partial_rejection",
                        producer_start.elapsed(),
                        batch_len,
                    );
                } else {
                    warn!(
                        producer_key = %key,
                        "system/callback produced events but no agent subscription matched; \
                         not advancing checkpoint (callbacks must not be reconciled as delivered)"
                    );
                    metrics::record_event_poll_producer(
                        &key,
                        "callback_no_subscribers",
                        producer_start.elapsed(),
                        batch_len,
                    );
                }
                results.push((key, Ok(aggregate)));
            }
        }

        metrics::record_event_poll_cycle(cycle_start.elapsed());
        results
    }
}
