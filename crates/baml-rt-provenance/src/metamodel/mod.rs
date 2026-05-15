//! Typed metamodel surface for the provenance graph.
//!
//! The submodules together turn the inert `MAPPING_*` constants in
//! [`crate::graph_model`] into a closed, compile-time enforced metamodel:
//!
//! - [`labels`]: ZST markers per [`crate::graph_model::GraphNodeLabel`] variant.
//! - [`node_ids`]: typed newtypes for on-disk node identifiers.
//! - [`keys`]: ZST property keys with subject-specific filter-key traits.
//!   `keys::ContextId` / `TaskId` / `AgentId` deliberately do not implement
//!   any subject's filter trait — context, task, and agent are EDGES.
//! - [`edges`]: closed [`edges::SemanticEdge`] enum + sealed
//!   [`edges::AllowedPrimaryEdge<E>`] witness traits. Per-(event, edge)
//!   blessings are encoded as ZST impls — no string strings, no fictional
//!   edges, no inert `&[&str]`.
//! - [`events`]: per-`EventGraphKind` ZST markers + the [`events::GraphEvent`]
//!   trait + nominal `RequiredProps` structs that eliminate the `Option`
//!   bag for required metamodel fields.
//! - [`query`]: typed [`query::GraphQuery<Subject, ScopeState>`] read DSL
//!   that makes property-as-relationship filters syntactically
//!   unrepresentable.
//! - [`writer`]: [`writer::MetamodelWriter<E>`] facade for normalizer arms.
//!
//! See [`crate::ConversationGraphTraversal`] for the canonical multi-hop
//! traversal paths these types encode.
//!
//! # Visibility convention
//!
//! Inside `crates/baml-rt-provenance/src/`, **every SurrealQL string that
//! targets the `prov_node` or `prov_edge` table must be produced by
//! [`query::GraphQuery::into_surreal`] or [`query::EdgeProjection::into_surreal`]**.
//! Hand-rolled multi-hop traversals built via
//! `format!`-of-`semantic_labels::WAS_*` bypass the metamodel entirely
//! and are prohibited.
//!
//! Concretely, the following imports are illegal *inside this crate's
//! `src/` tree* outside `metamodel/`:
//!
//! - `use crate::vocabulary::semantic_labels::*` (or any
//!   `WAS_*`/`SCOPED_TO`/`A2A_TASK_*` constant interpolation into a
//!   SurrealQL string)
//! - `use crate::vocabulary::a2a_relations::*` (same reason)
//! - `crate::graph_model::GraphNodeLabel::<Variant>::as_str()` interpolated
//!   into a SurrealQL string (label literals must come from
//!   `Subject::LABEL_STR` via `GraphQuery`)
//!
//! The `vocabulary` re-export in [`crate::lib`] remains `pub` for
//! cross-crate consumers (graph_export adapters, sequence diagram
//! renderers, etc.). The intra-crate fence is a project convention.
//!
//! The write-side helpers `surreal_write_batch.rs` and
//! `prov_write_semantics.rs` are currently exempt from the strictest
//! reading of this convention; their
//! migration to the typed [`writer::MetamodelWriter`] facade is tracked
//! separately.

mod sealed;

pub mod edges;
pub mod events;
pub mod keys;
pub mod labels;
pub mod node_ids;
pub mod query;
pub mod writer;

pub use edges::{AllowedPrimaryEdge, EdgeWitness, SemanticEdge};
pub use events::{
    A2ATaskStateProps, AgentBooted, AgentStopped, CallbackDispatchContextsLinked, EmptyStringError,
    ExternalToolLifecycle, GraphEvent, IntentResolved, LegacyRequiredProps, LlmCallCompleted,
    LlmCallStarted, MessageDirection, MessageReceived, MessageReceivedProps, MessageSent,
    MessageSentProps, NonEmptyString, PlanGenerated, PlanStepStatusChanged, PromptRejected,
    TaskArtifactGenerated, TaskExecutionEnded, TaskExecutionStarted, TaskExists, TaskStatusChanged,
    TaskStatusKind, ToolCallCompleted, ToolCallStarted, ToolSessionStep,
};
pub use node_ids::{
    AgentPackage, AgentRuntimeInstanceNodeId, ContextNodeId, MessageNodeId, ScopedTaskRef,
    TaskExecutionNodeId, TaskNodeId,
};
pub use query::{
    AgentStopFilterKey, EdgeProjection, FilterOp, GraphQuery, LlmCallFilterKey, MessageFilterKey,
    ScopeState, Scoped, ScopedToContext, SessionStepFilterKey, SortDir, SortKey, TaskFilterKey,
    ToolCallFilterKey, Unbounded, Unscoped,
};
pub use writer::{CommittedTypedFragment, MetamodelWriter, NodeEndpoint, TypedPrimaryEdge};
