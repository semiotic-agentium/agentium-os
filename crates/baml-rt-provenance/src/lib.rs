//! Provenance capture and storage.
//!
//! This crate provides event types and interceptors for provenance recording,
//! along with a pluggable storage interface and an in-memory implementation.

pub mod error;
pub mod events;
pub mod types;
pub mod document;
pub mod builders;
pub mod store;
pub mod interceptors;
pub mod normalizer;
pub mod falkordb_store;
pub mod tool_index;
pub mod vocabulary;
pub mod id_semantics;
pub mod effect_subscriber;

pub use error::ProvenanceError;
pub use events::{
    AgentType, CallScope, GlobalEvent, LlmUsage, ProvEvent, ProvEventData, TaskScopedEvent,
};
pub use store::{InMemoryProvenanceStore, ProvenanceWriter};
pub use interceptors::ProvenanceInterceptor;
pub use effect_subscriber::ProvenanceEffectSubscriber;
pub use normalizer::{
    normalize_event, validate_event, A2aDerivedRelation, A2aRelationType, DefaultProvNormalizer,
    NormalizedProv, ProvNormalizer,
};
pub use falkordb_store::{FalkorDbProvenanceConfig, FalkorDbProvenanceWriter};
pub use tool_index::{ToolIndexConfig, index_tools};
pub use types::{
    ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef,
};