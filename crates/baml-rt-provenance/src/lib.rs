//! Provenance capture and storage.
//!
//! This crate provides event types and interceptors for provenance recording,
//! along with a pluggable storage interface and GraphQLite-backed implementation.
//!
//! ## Design: No heuristics in projection
//!
//! **Heuristics, backfills, and fallbacks must never be used to project graphs.**
//! If a projection (export, query, render) is impossible given the stored graph,
//! the graph construction (write path) is incorrect. Fix the write path.

pub mod a2a_graph_event_recorder;
pub mod a2a_graph_store;
pub mod builders;
pub mod bus_subscriber;
pub mod cypher_build;
pub use cypher_build::{CypherStatement, KeyStyle};
pub mod context_metrics_queries;
pub mod document;
pub mod effect_subscriber;
pub mod error;
pub mod events;
pub mod graph_export;
pub mod graph_model;
pub mod mermaid_cache;
pub mod graph_store;
pub mod graphqlite_config;
pub mod graphqlite_store;
pub mod id_semantics;
pub mod interceptors;
pub mod normalizer;
pub mod spans;
pub mod store;
pub mod tool_index;
pub mod types;
pub mod vocabulary;

pub use a2a_graph_event_recorder::{
    A2aGraphEventRecorder, ArtifactUpdateContext, StatusUpdateContext, record_artifact_update,
    record_status_update,
};
pub use baml_rt_vocabulary::{A2aGraphStore, GraphStore, TaskSubgraphNode, TaskSubgraphUpdateNode};
pub use bus_subscriber::ProvenanceBusSubscriber;
pub use effect_subscriber::ProvenanceEffectSubscriber;
pub use error::ProvenanceError;
pub use events::{
    AgentBootedEvent, AgentType, CallScope, GlobalEvent, LlmUsage, ProvEvent, ProvEventData,
    TaskScopedEvent,
};
pub use graph_export::{ExportScope, ExportedGraph, GraphExporter};
pub use graph_model::{
    ALL_EVENT_KINDS, ConversationReadModel, EDGE_TASK_EMITTED_MESSAGE,
    EDGE_TASK_TRIGGERED_BY_MESSAGE,
    EDGE_WAS_CREATED_BY, EDGE_WAS_EMITTED_BY,
    EDGE_WAS_EXECUTED_BY, EDGE_WAS_GENERATED_BY, EDGE_WAS_INVOKED_BY, EDGE_WAS_RECEIVED_BY,
    EDGE_WAS_SPAWNED_BY, EDGE_WAS_TRANSITIONED_FROM, EDGE_WAS_UPDATED_BY, EDGE_WAS_USED_BY,
    EventGraphKind, EventGraphMapping, GraphNodeLabel, TOOL_CALL_ARGS_EDGE, event_kind_from_data,
    mapping_for_event_data, mapping_for_event_kind,
};
pub use graphqlite_config::{GraphqliteStoreConfig, StorePath};
pub use mermaid_cache::MermaidCache;
pub use graphqlite_store::{
    GraphCypherResult, GraphQueryParams, GraphRow, GraphqliteBackend, GraphqliteProvenanceStore,
    GraphqliteStoreBuilder,
};
pub use interceptors::ProvenanceInterceptor;
pub use normalizer::{
    A2aDerivedRelation, A2aRelationType, DefaultProvNormalizer, NormalizeContext, NormalizedProv,
    ProvNormalizer, normalize_event, task_entity_id_string, validate_event,
};
pub use store::{
    ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceQueryApi, ProvenanceReadIntent, ProvenanceWriter, ToolSessionPhase,
};
pub use tool_index::{ToolIndexConfig, index_tools, index_tools_into_connection};
pub use types::{ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef};
