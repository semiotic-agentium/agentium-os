//! Runtime contract for host-managed event producers.
//!
//! An [`EventProducer`] is a companion interface to [`ToolHandler`](crate::tools::ToolHandler).
//! A tool bundle that produces events registers both a handler (for invocations) and a producer
//! (for host-managed event intake). The [`ProducerRegistry`] holds registered producers and
//! their opaque checkpoint cursors.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, EventSourceKind, ProducedEvent, Result};
use serde_json::Value;
use tracing::warn;

use crate::{ConfigResolver, ToolCatalog, ToolName, tools::ToolFunctionMetadata};

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

/// Inputs provided when constructing configured event producers from inventory.
#[derive(Debug, Clone)]
pub struct EventProducerBuildContext {
    /// Tool metadata this producer operationalizes.
    pub metadata: ToolFunctionMetadata,
    /// Effective config for this tool after resolver/default merge.
    pub config: Option<Value>,
    /// Persisted checkpoints keyed by producer identity from prior runs.
    ///
    /// Providers can use this to reconcile durable state with current config
    /// before constructing producer instances. This keeps source-specific
    /// identity recovery out of the runner and leaves a clean seam for future
    /// host-native producers like `system/callback` or declarative providers.
    pub persisted_checkpoints: Arc<HashMap<String, ProducerCheckpoint>>,
}

pub type EventProducerBuildFuture =
    Pin<Box<dyn Future<Output = Result<Vec<Arc<dyn EventProducer>>>> + Send>>;

/// Inventory provider for host-managed event producers.
///
/// Providers are registered by the same crate that owns the source-capable tool
/// metadata. This keeps event production tool-native instead of runner-specific.
pub struct EventProducerProvider {
    /// Tool name whose `event_sources` this provider operationalizes.
    pub tool_name: &'static str,
    /// Build zero or more configured producer instances for this tool.
    pub build: fn(EventProducerBuildContext) -> EventProducerBuildFuture,
}

inventory::collect!(EventProducerProvider);

/// Build all configured event producers registered in inventory.
///
/// Each provider is keyed off tool metadata so discovery/config remain the
/// source of truth. Stored config overrides the metadata default when present.
pub async fn load_configured_event_producers<C: ToolCatalog>(
    catalog: &C,
    config_resolver: Option<Arc<dyn ConfigResolver>>,
) -> Result<Vec<Arc<dyn EventProducer>>> {
    load_configured_event_producers_with_checkpoints(catalog, config_resolver, HashMap::new()).await
}

pub async fn load_configured_event_producers_with_checkpoints<C: ToolCatalog>(
    catalog: &C,
    config_resolver: Option<Arc<dyn ConfigResolver>>,
    persisted_checkpoints: HashMap<String, ProducerCheckpoint>,
) -> Result<Vec<Arc<dyn EventProducer>>> {
    let mut producers = Vec::new();
    let persisted_checkpoints = Arc::new(persisted_checkpoints);

    for provider in inventory::iter::<EventProducerProvider> {
        let tool_name = ToolName::parse(provider.tool_name)?;
        let Some(metadata) = catalog.by_name(&tool_name).cloned() else {
            warn!(
                provider = provider.tool_name,
                "event producer provider has no matching tool metadata; skipping"
            );
            continue;
        };

        if metadata.event_sources.is_empty() {
            return Err(BamlRtError::InvalidArgument(format!(
                "event producer provider '{}' targets tool '{}' which declares no event_sources",
                provider.tool_name, metadata.name
            )));
        }

        let config = load_effective_config(&metadata, config_resolver.as_ref()).await?;
        let built = (provider.build)(EventProducerBuildContext {
            metadata: metadata.clone(),
            config,
            persisted_checkpoints: Arc::clone(&persisted_checkpoints),
        })
        .await?;

        for producer in built {
            let undeclared: Vec<String> = producer
                .source_kinds()
                .iter()
                .filter(|kind| !metadata.event_sources.contains(kind))
                .map(|kind| kind.as_str().to_string())
                .collect();
            if !undeclared.is_empty() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "event producer '{}' for tool '{}' emits undeclared source_kinds: {}",
                    producer.producer_key(),
                    metadata.name,
                    undeclared.join(", ")
                )));
            }
            producers.push(producer);
        }
    }

    Ok(producers)
}

async fn load_effective_config(
    metadata: &ToolFunctionMetadata,
    config_resolver: Option<&Arc<dyn ConfigResolver>>,
) -> Result<Option<Value>> {
    let default = metadata.config.as_ref().map(|meta| meta.default.clone());

    match (
        config_resolver,
        metadata.config_bundle.as_ref(),
        metadata.config.as_ref(),
    ) {
        (Some(resolver), Some(bundle_name), Some(_)) => {
            match resolver.get_config_with_version(bundle_name).await {
                Ok(config) => Ok(config.map(|(config, _version)| config).or(default)),
                Err(err) => {
                    warn!(
                        bundle = %bundle_name.as_str(),
                        error = %err,
                        "failed to load event producer config; falling back to metadata default"
                    );
                    Ok(default)
                }
            }
        }
        _ => Ok(default),
    }
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
        self.register_with_checkpoint(producer, ProducerCheckpoint::none())
    }

    /// Register a producer with an existing checkpoint.
    pub fn register_with_checkpoint(
        &mut self,
        producer: Arc<dyn EventProducer>,
        checkpoint: ProducerCheckpoint,
    ) -> Result<()> {
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
        self.checkpoints.insert(key, checkpoint);
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
    use std::{collections::HashMap, sync::Arc};

    use async_trait::async_trait;
    use baml_rt_core::{BamlRtError, EventSourceKind, Result};
    use serde_json::{Value, json};

    use super::{
        ConfigResolver, EventProducer, ProducerCheckpoint, ProducerPoll, ProducerRegistry,
        load_configured_event_producers_with_checkpoints, load_effective_config,
    };
    use crate::{
        BundleName, ToolCatalog, ToolName, ToolOrigin, ToolTypeSpec,
        tools::{SessionPolicy, ToolConfigMetadata, ToolFunctionMetadata},
    };

    struct FakeProducer {
        key: String,
        kinds: Vec<EventSourceKind>,
    }

    #[derive(Clone)]
    enum StubResolverResult {
        Value(Option<(Value, u64)>),
        Error(String),
    }

    #[derive(Clone)]
    struct StubResolver {
        result: StubResolverResult,
    }

    #[async_trait]
    impl ConfigResolver for StubResolver {
        async fn get_config(&self, bundle_name: &BundleName) -> Result<Option<Value>> {
            Ok(self
                .get_config_with_version(bundle_name)
                .await?
                .map(|(value, _version)| value))
        }

        async fn get_config_with_version(
            &self,
            _bundle_name: &BundleName,
        ) -> Result<Option<(Value, u64)>> {
            match &self.result {
                StubResolverResult::Value(value) => Ok(value.clone()),
                StubResolverResult::Error(message) => {
                    Err(BamlRtError::Io(std::io::Error::other(message.clone())))
                }
            }
        }
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

    fn metadata_with_default_config(default: Value) -> ToolFunctionMetadata {
        ToolFunctionMetadata {
            name: ToolName::parse("support/test").expect("valid tool name"),
            class_name: "SupportTest".to_string(),
            description: "test".to_string(),
            open_input_schema: json!({}),
            input_schema: json!({}),
            output_schema: json!({}),
            open_input_type: ToolTypeSpec {
                name: "TestOpenInput".to_string(),
                ts_decl: None,
            },
            input_type: ToolTypeSpec {
                name: "TestInput".to_string(),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: "TestOutput".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: vec![],
            access: None,
            tags: vec![],
            secret_requests: vec![],
            config: Some(ToolConfigMetadata::new(
                json!({"type":"object"}),
                default,
                None,
            )),
            config_bundle: Some(BundleName::new("support_test").expect("valid bundle")),
            origin: ToolOrigin::Host,
            backend: crate::tools::ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: SessionPolicy::Strict,
            event_sources: vec![],
            coordination_baml: None,
        }
    }

    struct EmptyCatalog;

    impl ToolCatalog for EmptyCatalog {
        fn by_name(&self, _name: &ToolName) -> Option<&ToolFunctionMetadata> {
            None
        }

        fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
            Box::new(std::iter::empty())
        }
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

    #[test]
    fn register_with_existing_checkpoint() {
        let mut registry = ProducerRegistry::new();
        registry
            .register_with_checkpoint(
                make_producer("test:e", &["slack"]),
                ProducerCheckpoint::some("cursor-2"),
            )
            .unwrap();

        assert_eq!(registry.checkpoint("test:e").value(), Some("cursor-2"));
    }

    #[tokio::test]
    async fn load_effective_config_falls_back_to_default_on_read_error() {
        let metadata = metadata_with_default_config(json!({"channels": []}));
        let resolver: Arc<dyn ConfigResolver> = Arc::new(StubResolver {
            result: StubResolverResult::Error("config read failed".to_string()),
        });

        let config = load_effective_config(&metadata, Some(&resolver))
            .await
            .expect("fallback to default config");

        assert_eq!(config, Some(json!({"channels": []})));
    }

    #[tokio::test]
    async fn load_effective_config_uses_store_value_when_available() {
        let metadata = metadata_with_default_config(json!({"channels": []}));
        let resolver: Arc<dyn ConfigResolver> = Arc::new(StubResolver {
            result: StubResolverResult::Value(Some((json!({"channels": ["ops"]}), 7))),
        });

        let config = load_effective_config(&metadata, Some(&resolver))
            .await
            .expect("config load succeeds");

        assert_eq!(config, Some(json!({"channels": ["ops"]})));
    }

    #[tokio::test]
    async fn load_configured_event_producers_skips_missing_catalog_entries() {
        let producers =
            load_configured_event_producers_with_checkpoints(&EmptyCatalog, None, HashMap::new())
                .await
                .expect("missing metadata should be skipped");

        assert!(producers.is_empty());
    }
}
