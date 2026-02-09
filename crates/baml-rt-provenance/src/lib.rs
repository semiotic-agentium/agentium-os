//! Provenance capture and storage.
//!
//! This crate provides event types and interceptors for provenance recording,
//! along with a pluggable storage interface and an in-memory implementation.

pub mod builders;
pub mod document;
pub mod effect_subscriber;
pub mod error;
pub mod events;
pub mod falkordb_store;
pub mod id_semantics;
pub mod interceptors;
pub mod normalizer;
pub mod store;
pub mod tool_index;
pub mod types;
pub mod vocabulary;

pub use effect_subscriber::ProvenanceEffectSubscriber;
pub use error::ProvenanceError;
pub use events::{
    AgentType, CallScope, GlobalEvent, LlmUsage, ProvEvent, ProvEventData, TaskScopedEvent,
};
pub use falkordb_store::{FalkorDbProvenanceConfig, FalkorDbProvenanceWriter};
pub use interceptors::ProvenanceInterceptor;
pub use normalizer::{
    A2aDerivedRelation, A2aRelationType, DefaultProvNormalizer, NormalizedProv, ProvNormalizer,
    normalize_event, validate_event,
};
pub use store::{InMemoryProvenanceStore, ProvenanceWriter};
pub use tool_index::{ToolIndexConfig, index_tools};
pub use types::{ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef};
