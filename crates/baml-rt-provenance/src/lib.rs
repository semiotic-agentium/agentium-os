//! Provenance capture and storage.
//!
//! This crate provides event types and interceptors for provenance recording,
//! along with a pluggable storage interface and SurrealDB-backed implementation.
//!
//! ## Design: No heuristics in projection
//!
//! **Heuristics, backfills, and fallbacks must never be used to project graphs.**
//! If a projection (export, query, render) is impossible given the stored graph,
//! the graph construction (write path) is incorrect. Fix the write path.

pub mod a2a_graph_event_recorder;
pub mod builders;
pub mod bus_subscriber;
pub mod citation_queries;
pub mod context_metrics_queries;
pub mod conversation_projection;
pub mod document;
pub mod effect_subscriber;
pub mod episode;
pub mod error;
pub mod events;
pub mod graph_export;
pub mod graph_model;
pub mod id_semantics;
pub mod interceptors;
pub mod mermaid_cache;
pub mod normalizer;
pub(crate) mod payload_id;
pub(crate) mod payload_record;
pub(crate) mod payload_storage;
pub(crate) mod prov_write_semantics;
pub mod spans;
pub mod store;
pub mod surreal_config;
pub(crate) mod surreal_sql;
pub mod surreal_store;
pub(crate) mod surreal_tables;
pub(crate) mod surreal_write_batch;
pub mod tool_index;
pub mod types;
pub mod vocabulary;

pub use a2a_graph_event_recorder::{
    A2aGraphEventRecorder, ArtifactUpdateContext, StatusUpdateContext, record_artifact_update,
    record_status_update,
};
pub use baml_rt_vocabulary::{
    A2aGraphStore, A2aGraphStoreError, A2aGraphStoreResult, TaskSubgraphNode,
    TaskSubgraphUpdateNode,
};
pub use bus_subscriber::ProvenanceBusSubscriber;
pub use conversation_projection::provenance_item_to_projection_item;
pub use effect_subscriber::ProvenanceEffectSubscriber;
pub use episode::{
    ArtifactSummary, CachedEpisode, Episode, EpisodeArchiveSource, EpisodeContent,
    EpisodeDriftCall, EpisodeDriftSummary, EpisodeDuration, EpisodeEntry, EpisodeOutcome,
    EpisodeReader, EpisodeRefPrefix, IntentRevision, PlanRevision, PlanStepEntry,
    SessionHistoryLine, StepType, TerminalStatus, TokenSummary, aggregate_task_drift,
    episode_ref_table, prefix_wire_citation, render_episode,
};
pub use error::ProvenanceError;
pub use events::{
    AgentBootedEvent, AgentType, CallScope, GlobalEvent, LlmUsage, PlanStepSpec, ProvEvent,
    ProvEventData, ReservedAnchor, TaskScopedEvent, allocate_activity_anchor,
};
pub use graph_export::{ExportScope, ExportedGraph, GraphExporter};
pub use graph_model::{
    ALL_EVENT_KINDS, ConversationReadModel, EDGE_TASK_EMITTED_MESSAGE,
    EDGE_TASK_TRIGGERED_BY_MESSAGE, EDGE_WAS_CLASSIFIED_BY, EDGE_WAS_CREATED_BY,
    EDGE_WAS_EMITTED_BY, EDGE_WAS_EXECUTED_BY, EDGE_WAS_GENERATED_BY, EDGE_WAS_INVOKED_BY,
    EDGE_WAS_RECEIVED_BY, EDGE_WAS_SPAWNED_BY, EDGE_WAS_TRANSITIONED_FROM, EDGE_WAS_UPDATED_BY,
    EDGE_WAS_USED_BY, EventGraphKind, EventGraphMapping, GraphNodeLabel, TOOL_CALL_ARGS_EDGE,
    event_kind_from_data, mapping_for_event_data, mapping_for_event_kind,
};
pub use interceptors::ProvenanceInterceptor;
pub use mermaid_cache::MermaidCache;
pub use normalizer::{
    A2aDerivedRelation, A2aRelationType, DefaultProvNormalizer, NormalizeContext, NormalizedProv,
    ProvNormalizer, normalize_event, plan_entity_id_string, task_entity_id_string, validate_event,
};
pub use store::{
    ActivityRef, ArchiveRef, PayloadRef, PlanningIntentRecord, PlanningPlanRecord,
    PlanningPlanStepRecord, ProvenanceArchivePayload, ProvenanceArchiveRecord,
    ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
    ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest,
    ProvenanceOpsQueryResponse, ProvenanceOpsResource, ProvenanceOutcomeSegment,
    ProvenancePlanningQuery, ProvenanceQueryApi, ProvenanceReadIntent, ProvenanceResponseProfile,
    ProvenanceWriter, ToolSessionPhase,
};
pub use surreal_config::SurrealStoreConfig;
pub use surreal_store::{SurrealBackend, SurrealProvenanceStore, SurrealStoreBuilder};
pub use tool_index::{ToolIndexConfig, index_tools};
pub use types::{ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef};
