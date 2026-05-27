// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
//!
//! Agent-visible **conversation row types** (`ProvenanceConversationContextItem`, tool shells,
//! etc.) and pure projection live in [`baml_rt_conversation`]; this crate re-exports only
//! storage traits and I/O. Import view types from `baml_rt_conversation::view`, not from here.
//!
//! ## Visibility convention: typed metamodel is the only graph-SQL emitter
//!
//! Inside this crate's `src/` tree, **every SurrealQL string targeting
//! `prov_node` or `prov_edge` MUST be produced by
//! [`metamodel::GraphQuery::into_surreal`] or
//! [`metamodel::EdgeProjection::into_surreal`]**. Hand-rolled multi-hop
//! traversals via `format!`-of-`vocabulary::semantic_labels::WAS_*` (or
//! `a2a_relations::*` / `GraphNodeLabel::<X>::as_str()`) bypass the
//! metamodel entirely and are prohibited.
//!
//! The `vocabulary` module remains `pub` for cross-crate consumers
//! (graph_export adapters, sequence diagram renderers, simplifiers) that
//! need the human-readable label strings for non-SQL purposes.
//! Write-side helpers (`surreal_write_batch.rs`, `prov_write_semantics.rs`)
//! are temporarily exempted from this convention pending the
//! [`metamodel::MetamodelWriter`] facade migration in a follow-on phase.

pub mod builders;
pub mod bus_subscriber;
pub mod citation_queries;
pub mod context_metrics_queries;
/// Defaults for LLM conversation-context query limits.
pub mod conversation_context_query;
pub mod conversation_history_resume;
pub mod document;
pub mod effect_subscriber;
pub mod episode;
pub mod error;
pub mod events;
pub mod graph_export;
pub mod graph_model;
pub mod host_ingress_identity;
pub mod host_ingress_recorder_impl;
pub mod host_ingress_transcript;
pub mod host_ingress_types;
pub use host_ingress_types::{
    HostDispatchFailureKind, HostDispatchRejectedSpec, HostIngressKind, HostIngressSourceRef,
};
pub mod id_semantics;
pub mod interceptors;
pub mod mermaid_cache;
/// Typed metamodel surface for the provenance graph.
///
/// Lifts the inert `MAPPING_*` constants in [`graph_model`] into a closed,
/// compile-time enforced type system, and provides the only legal SQL-emission
/// surface (`GraphQuery`, `EdgeProjection`) inside this crate's `src/` tree.
/// See [`metamodel`] module docs for the visibility convention and the
/// per-submodule responsibilities.
pub mod metamodel;
pub mod normalizer;
pub mod observation;
pub(crate) mod payload_id;
pub(crate) mod payload_record;
pub(crate) mod payload_storage;
pub(crate) mod prov_write_semantics;
pub mod read;
pub mod spans;
pub mod store;
pub mod surreal_config;
pub(crate) mod surreal_sql;
pub mod surreal_store;
pub(crate) mod surreal_tables;
pub(crate) mod surreal_write_batch;
pub mod task_agent_binding;
pub mod task_graph_reader;
pub mod tool_index;
pub mod types;
pub mod vocabulary;

pub use baml_rt_conversation::provenance_item_to_projection_item;
pub use bus_subscriber::ProvenanceBusSubscriber;
pub use conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP;
pub use conversation_history_resume::{ConversationResumeUiHints, resolve_resume_ui_hints};
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
    AgentBootedEvent, AgentStoppedEvent, AgentType, CallScope, GlobalEvent, LlmUsage, PlanStepSpec,
    ProvEvent, ProvEventData, ReservedAnchor, TaskScopedEvent, ToolSessionStepOpKind,
    allocate_activity_anchor, serialized_prompt_utf8_len,
};
pub use graph_export::{ExportScope, ExportedGraph, GraphExporter};
pub use graph_model::{
    ALL_EVENT_KINDS, ConversationGraphTraversal, ConversationReadModel, EDGE_A2A_TASK_MESSAGE,
    EDGE_WAS_ASSOCIATED_WITH, EDGE_WAS_CLASSIFIED_BY, EDGE_WAS_CREATED_BY, EDGE_WAS_EMITTED_BY,
    EDGE_WAS_EXECUTED_BY, EDGE_WAS_GENERATED_BY, EDGE_WAS_INVOKED_BY, EDGE_WAS_RECEIVED_BY,
    EDGE_WAS_SPAWNED_BY, EDGE_WAS_TRANSITIONED_FROM, EDGE_WAS_UPDATED_BY, EDGE_WAS_USED_BY,
    EventGraphKind, EventGraphMapping, GraphNodeLabel, TOOL_CALL_ARGS_EDGE, event_kind_from_data,
    mapping_for_event_data, mapping_for_event_kind,
};
pub use host_ingress_recorder_impl::HostIngressRecorderImpl;
pub use interceptors::ProvenanceInterceptor;
pub use mermaid_cache::MermaidCache;
pub use normalizer::{
    A2aDerivedRelation, A2aRelationType, DefaultProvNormalizer, NormalizeContext, NormalizedProv,
    ProvNormalizer, normalize_event, plan_entity_id_string, task_entity_id_string, validate_event,
};
pub use observation::{
    EventOrder, LoadedObservation, ObservationLoader, ObservationScope, ObservationVersion,
    OpsQueryMode, PageVersionEnvelope, PromptOpsVersionRow, ResumeVersionHints,
    TaskObservationMetrics, TaskObservationScope, TemporalBound, cmp_transcript_items,
    hash_page_envelope, observation_scope_from_history, observation_scope_from_ops_filters,
    observation_version_from_hasher, observation_version_from_loaded, observation_version_page,
    observation_version_transcript, sort_transcript_items, task_ids_for_context,
    task_ids_for_scope, transcript_delta_rows,
};
pub use read::{OpsPageSpec, OpsReader, PlanningReader, TranscriptSlice, TranscriptSliceSpec};
pub use store::{
    ActivityRef, ArchiveRef, PayloadRef, PlanningIntentRecord, PlanningPlanRecord,
    PlanningPlanStepRecord, ProvenanceArchivePayload, ProvenanceArchiveRecord,
    ProvenanceContextReader, ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest,
    ProvenanceOpsQueryResponse, ProvenanceOpsResource, ProvenanceOutcomeSegment,
    ProvenancePlanningQuery, ProvenanceQueryApi, ProvenanceReadIntent, ProvenanceResponseProfile,
    ProvenanceWriter, TaskAgentResolution,
};
pub use surreal_config::SurrealStoreConfig;
pub use surreal_store::{
    ContextPickerIndexRow, RemoteConfig, RemoteCredentials, SurrealBackend, SurrealProvenanceStore,
    SurrealStoreBuilder, TranscriptReader, hydrate_ref_table, prepare_ref_table_for_projection,
};
pub use task_agent_binding::{
    TaskAgentBinding, TaskAgentBindingSource, event_local_executing_agent_id,
    is_unassigned_executing_agent,
};
pub use task_graph_reader::{
    ArtifactRef, HydratedTask, MessageRef, ReplayError, TaskGraphReader, TaskReplayCursor,
    TaskReplayCursorError, TaskReplayEvent, TaskUpdateFrame,
};
pub use tool_index::{ToolIndexConfig, index_tools};
pub use types::{ProvActivityId, ProvAgentId, ProvEntityId, ProvNodeRef};
