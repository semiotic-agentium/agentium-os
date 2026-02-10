//! Provenance capture and storage.
//!
//! This crate provides event types and interceptors for provenance recording,
//! along with a pluggable storage interface and FalkorDB-backed implementation.

pub mod builders;
pub mod document;
pub mod effect_subscriber;
pub mod error;
pub mod events;
pub mod falkordb_store;
pub mod graph_model;
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
pub use graph_model::{
    ALL_EVENT_KINDS, ConversationReadModel, EDGE_WAS_CREATED_BY, EDGE_WAS_EMITTED_BY,
    EDGE_WAS_EXECUTED_BY, EDGE_WAS_GENERATED_BY, EDGE_WAS_INVOKED_BY, EDGE_WAS_RECEIVED_BY,
    EDGE_WAS_SPAWNED_BY, EDGE_WAS_TRANSITIONED_FROM, EDGE_WAS_UPDATED_BY, EDGE_WAS_USED_BY,
    EventGraphKind, EventGraphMapping, GraphNodeLabel, TOOL_CALL_ARGS_EDGE, event_kind_from_data,
    mapping_for_event_data, mapping_for_event_kind,
};
pub use interceptors::ProvenanceInterceptor;
pub use normalizer::{
    A2aDerivedRelation, A2aRelationType, DefaultProvNormalizer, NormalizedProv, ProvNormalizer,
    normalize_event, validate_event,
};
pub use store::{
    ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceWriter, ToolSessionPhase,
};
pub use tool_index::{ToolIndexConfig, index_tools};
pub use types::{ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef};
