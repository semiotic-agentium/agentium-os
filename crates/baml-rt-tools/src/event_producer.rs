//! Runtime contract for host-managed event producers.
//!
//! An [`EventProducer`] is a companion interface to [`ToolHandler`](crate::tools::ToolHandler).
//! A tool bundle that produces events registers both a handler (for invocations) and a producer
//! (for host-managed event intake). The [`ProducerRegistry`] holds registered producers and
//! their opaque checkpoint cursors.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{EventSourceKind, ProducedEvent, Result};
use tracing::warn;

/// Opaque checkpoint cursor for a producer's poll state.
///
/// Producers own the format; the host stores and returns it opaquely.
/// `None` means no prior checkpoint (first poll).
#[derive(Debug, Clone, Default)]
pub struct ProducerCheckpoint(pub Option<String>);

impl ProducerCheckpoint {
    /// Create a checkpoint with a value.
    pub fn some(value: impl Into<String>) -> Self {
        Self(Some(value.into()))
    }

    /// Create an empty checkpoint (first poll).
    pub fn none() -> Self {
        Self(None)
    }

    /// The checkpoint value, if any.
    pub fn value(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// Output of one poll cycle from a producer.
#[derive(Debug, Clone)]
pub struct ProducerPoll {
    /// Events produced in this cycle (may be empty if the source had nothing new).
    pub events: Vec<ProducedEvent>,
    /// Updated checkpoint to persist after successful delivery.
    /// `None` inner value means "do not advance" (e.g. empty poll).
    pub checkpoint: ProducerCheckpoint,
}

/// Runtime contract for a host-managed event producer.
///
/// Poll-based producers implement [`poll`](EventProducer::poll). The host calls it
/// periodically and delivers the resulting events to subscribed agents. Push-based
/// producers (webhooks, callbacks) will use a separate entrypoint in a future extension;
/// the trait has a clean seam for that.
#[async_trait]
pub trait EventProducer: Send + Sync {
    /// Stable identifier for this producer instance (e.g. `slack:C_agentium-eng`).
    ///
    /// Used as the key for checkpoint persistence. Must be unique across all
    /// registered producers.
    fn producer_key(&self) -> &str;

    /// Event source kinds this producer emits (e.g. `["slack"]`).
    ///
    /// Must be non-empty and should match the `event_sources` declared on the
    /// corresponding tool metadata.
    fn source_kinds(&self) -> &[EventSourceKind];

    /// Poll for new events, given the last persisted checkpoint.
    ///
    /// Returns zero or more events plus an updated checkpoint. The host persists
    /// the checkpoint only after successful delivery of all events — this preserves
    /// at-least-once semantics.
    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll>;
}

/// Holds registered producers and their checkpoint cursors.
pub struct ProducerRegistry {
    producers: Vec<Arc<dyn EventProducer>>,
    checkpoints: HashMap<String, ProducerCheckpoint>,
}

impl ProducerRegistry {
    pub fn new() -> Self {
        Self {
            producers: Vec::new(),
            checkpoints: HashMap::new(),
        }
    }

    /// Register a producer. Returns an error if source_kinds is empty or the
    /// producer_key is already registered.
    pub fn register(&mut self, producer: Arc<dyn EventProducer>) -> Result<()> {
        let key = producer.producer_key().to_string();
        if producer.source_kinds().is_empty() {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "producer {key} declares no source_kinds"
            )));
        }
        if self.checkpoints.contains_key(&key) {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "producer {key} is already registered"
            )));
        }
        self.checkpoints.insert(key, ProducerCheckpoint::none());
        self.producers.push(producer);
        Ok(())
    }

    /// Registered producers.
    pub fn producers(&self) -> &[Arc<dyn EventProducer>] {
        &self.producers
    }

    /// Get the current checkpoint for a producer.
    pub fn checkpoint(&self, producer_key: &str) -> ProducerCheckpoint {
        self.checkpoints
            .get(producer_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Advance the checkpoint for a producer after successful delivery.
    pub fn advance_checkpoint(&mut self, producer_key: &str, checkpoint: ProducerCheckpoint) {
        match self.checkpoints.get_mut(producer_key) {
            Some(entry) => {
                // Only advance if the new checkpoint has a value.
                if checkpoint.0.is_some() {
                    *entry = checkpoint;
                }
            }
            None => {
                warn!(
                    producer_key = %producer_key,
                    "attempted to advance checkpoint for unregistered producer"
                );
            }
        }
    }
}

impl Default for ProducerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use baml_rt_core::EventSourceKind;

    use super::{EventProducer, ProducerCheckpoint, ProducerPoll, ProducerRegistry};

    struct FakeProducer {
        key: String,
        kinds: Vec<EventSourceKind>,
    }

    #[async_trait]
    impl EventProducer for FakeProducer {
        fn producer_key(&self) -> &str {
            &self.key
        }
        fn source_kinds(&self) -> &[EventSourceKind] {
            &self.kinds
        }
        async fn poll(
            &self,
            _checkpoint: &ProducerCheckpoint,
        ) -> baml_rt_core::Result<ProducerPoll> {
            Ok(ProducerPoll {
                events: vec![],
                checkpoint: ProducerCheckpoint::none(),
            })
        }
    }

    fn make_producer(key: &str, kinds: &[&str]) -> Arc<dyn EventProducer> {
        Arc::new(FakeProducer {
            key: key.into(),
            kinds: kinds.iter().filter_map(EventSourceKind::parse).collect(),
        })
    }

    #[test]
    fn register_and_retrieve_checkpoint() {
        let mut registry = ProducerRegistry::new();
        registry
            .register(make_producer("test:a", &["slack"]))
            .unwrap();

        assert!(registry.checkpoint("test:a").value().is_none());

        registry.advance_checkpoint("test:a", ProducerCheckpoint::some("cursor-1"));
        assert_eq!(registry.checkpoint("test:a").value(), Some("cursor-1"));
    }

    #[test]
    fn register_rejects_empty_source_kinds() {
        let mut registry = ProducerRegistry::new();
        let result = registry.register(make_producer("test:b", &[]));
        assert!(result.is_err());
    }

    #[test]
    fn register_rejects_duplicate_key() {
        let mut registry = ProducerRegistry::new();
        registry
            .register(make_producer("test:c", &["slack"]))
            .unwrap();
        let result = registry.register(make_producer("test:c", &["clickup"]));
        assert!(result.is_err());
    }

    #[test]
    fn advance_checkpoint_ignores_none_value() {
        let mut registry = ProducerRegistry::new();
        registry
            .register(make_producer("test:d", &["slack"]))
            .unwrap();
        registry.advance_checkpoint("test:d", ProducerCheckpoint::some("v1"));
        // Advancing with None should not overwrite.
        registry.advance_checkpoint("test:d", ProducerCheckpoint::none());
        assert_eq!(registry.checkpoint("test:d").value(), Some("v1"));
    }
}
