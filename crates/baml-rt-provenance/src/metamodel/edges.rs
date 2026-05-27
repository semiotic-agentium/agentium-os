//! Closed enumeration of edge labels (`SemanticEdge`) plus the sealed
//! `AllowedPrimaryEdge<E>` witness pattern.
//!
//! This module is the single place where edge label string literals live
//! (inside `SemanticEdge::as_rel_str`). All other call sites — read query
//! emission, write site selection, tests — must use the `SemanticEdge`
//! variant or one of the per-event witness types, so a future PR cannot
//! re-introduce a string typo or a Message-arm `WAS_INVOKED_BY`
//! misattribution.
//!
//! ## Why per-`(Event, Edge)` witness types
//!
//! A naive design would put `expected_edges` on a `GraphEvent::EXPECTED_EDGES:
//! &[SemanticEdge]` constant and ask the writer to assert membership at
//! runtime. That keeps the metamodel inert. Instead, each (Event, Edge) pair
//! that the metamodel blesses is materialised as a ZST that implements
//! [`AllowedPrimaryEdge<E>`] for that specific event marker. Because the
//! impls are sealed and finite, `Writer::<MessageReceived>::record_primary_edge::<W>`
//! requires `W: AllowedPrimaryEdge<MessageReceived>`, and the only way to
//! satisfy that bound is to use a witness whose impl exists in this file.
//!
//! Adding a new edge means: add an impl block here. Removing one means
//! deleting an impl. The metamodel is then enforced by method resolution.

use crate::{
    graph_model::{
        EDGE_A2A_TASK_MESSAGE, EDGE_WAS_ASSOCIATED_WITH, EDGE_WAS_CLASSIFIED_BY,
        EDGE_WAS_CREATED_BY, EDGE_WAS_EMITTED_BY, EDGE_WAS_EXECUTED_BY, EDGE_WAS_GENERATED_BY,
        EDGE_WAS_INVOKED_BY, EDGE_WAS_LAST_EXECUTED_BY, EDGE_WAS_LAST_TRANSITIONED_TO,
        EDGE_WAS_RECEIVED_BY, EDGE_WAS_SPAWNED_BY, EDGE_WAS_TRANSITIONED_FROM, EDGE_WAS_UPDATED_BY,
        EDGE_WAS_USED_BY,
    },
    metamodel::{
        events::{self, GraphEvent},
        sealed::Sealed,
    },
    vocabulary::{a2a_relations, semantic_labels},
};

/// Closed enumeration of all semantic edge labels referenced by metamodel
/// mappings. The on-disk `prov_edge.rel_type` values for derived A2A_*
/// relations live in [`crate::vocabulary::a2a_relations`] and are exposed
/// here through the `A2A_TASK_MESSAGE` variant where the metamodel uses
/// them.
///
/// Adding a variant requires adding the corresponding string in
/// [`Self::as_rel_str`] AND adding an `AllowedPrimaryEdge<E>` witness for
/// every (event, edge) pair the metamodel blesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticEdge {
    WasUsedBy,
    WasReceivedBy,
    WasEmittedBy,
    WasGeneratedBy,
    WasCreatedBy,
    WasExecutedBy,
    WasInvokedBy,
    WasUpdatedBy,
    WasTransitionedFrom,
    WasSpawnedBy,
    WasBootstrappedBy,
    WasAssociatedWith,
    WasClassifiedBy,
    /// `A2A_TASK_MESSAGE` — derived edge `Task → Message` with `direction`
    /// attribute. Single canonical label; the older speculative
    /// `TASK_TRIGGERED_BY_MESSAGE` / `TASK_EMITTED_MESSAGE` labels are
    /// deliberately not represented here.
    A2aTaskMessage,
    /// `A2A_TASK_CALL` — derived edge `A2ATaskExecution → LlmCall|ToolCall`.
    A2aTaskCall,
    /// `A2A_MESSAGE_CALL` — derived edge `A2AMessageProcessing →
    /// LlmCall|ToolCall`. Conceptual W3C-PROV equivalent of
    /// `WAS_INVOKED_BY` (LlmCall) / `WAS_EXECUTED_BY` (ToolCall) — the
    /// on-disk `rel_type` is `A2A_MESSAGE_CALL`. Used by the typed
    /// activity-to-agent traversal so that message-scoped LLM/tool calls
    /// route through `MessageProcessing -[WAS_EXECUTED_BY]->
    /// AgentRuntimeInstance` symmetrically with the task-scoped path
    /// through `A2A_TASK_CALL`.
    A2aMessageCall,
    /// `A2A_TASK_SESSION_STEP` — derived edge `A2ATask → SessionStep`.
    A2aTaskSessionStep,
    /// `A2A_TASK_ARTIFACT` — derived edge `A2ATask → Artifact`. Read-only
    /// at the typed surface (no `AllowedPrimaryEdge<E>` impl); written by
    /// the normalizer's `TaskArtifactGenerated` arm via the dynamic write
    /// batch.
    A2aTaskArtifact,
    /// `WAS_LAST_TRANSITIONED_TO` — head-pointer edge `A2ATask →
    /// A2ATaskState` naming the head of the immutable
    /// `WAS_TRANSITIONED_FROM` chain. Re-pointed atomically by the
    /// normalizer on every `TaskStatusChanged`. Cardinality (one per
    /// Task) is enforced by a UNIQUE index on `(rel_type, from_id)`
    /// filtered to this rel_type.
    WasLastTransitionedTo,
    /// `WAS_LAST_EXECUTED_BY` — head-pointer edge `A2ATask →
    /// AgentRuntimeInstance` naming the most-recent execution-owning
    /// agent. Collapses agent-identity lookup from a `Task → TaskExecution
    /// → AgentRuntimeInstance` two-hop traversal to a single indexed edge
    /// hop. Re-pointed atomically by the normalizer on every
    /// `TaskExecutionStarted`.
    WasLastExecutedBy,
    /// `SCOPED_TO` — context-scope edge written by the normalizer for
    /// every node that lives inside a `Context` (Task, Message,
    /// Artifact, ...). Read-only at this surface: scope-ed reads
    /// usually flow through [`crate::metamodel::query::ScopedToContext`]
    /// (a typed sub-query rather than a raw `EdgeProjection`); this
    /// variant exists for the rare reverse direction — given a node
    /// id, walk the `SCOPED_TO` edge to discover its owning context
    /// (used by [`crate::TaskGraphReader::resolve_by_task_id`]).
    ScopedTo,
}

impl SemanticEdge {
    /// On-disk `prov_edge.rel_type` string for this edge. The only place
    /// where these literal strings exist; all read/write call sites must
    /// route through this method.
    pub const fn as_rel_str(self) -> &'static str {
        match self {
            Self::WasUsedBy => semantic_labels::WAS_USED_BY,
            Self::WasReceivedBy => semantic_labels::WAS_RECEIVED_BY,
            Self::WasEmittedBy => semantic_labels::WAS_EMITTED_BY,
            Self::WasGeneratedBy => semantic_labels::WAS_GENERATED_BY,
            Self::WasCreatedBy => semantic_labels::WAS_CREATED_BY,
            Self::WasExecutedBy => semantic_labels::WAS_EXECUTED_BY,
            Self::WasInvokedBy => semantic_labels::WAS_INVOKED_BY,
            Self::WasUpdatedBy => semantic_labels::WAS_UPDATED_BY,
            Self::WasTransitionedFrom => semantic_labels::WAS_TRANSITIONED_FROM,
            Self::WasSpawnedBy => semantic_labels::WAS_SPAWNED_BY,
            Self::WasBootstrappedBy => semantic_labels::WAS_BOOTSTRAPPED_BY,
            Self::WasAssociatedWith => crate::vocabulary::prov_relations::WAS_ASSOCIATED_WITH,
            Self::WasClassifiedBy => semantic_labels::WAS_CLASSIFIED_BY,
            Self::A2aTaskMessage => EDGE_A2A_TASK_MESSAGE,
            Self::A2aTaskCall => a2a_relations::TASK_CALL,
            Self::A2aMessageCall => a2a_relations::MESSAGE_CALL,
            Self::A2aTaskSessionStep => a2a_relations::TASK_SESSION_STEP,
            Self::A2aTaskArtifact => a2a_relations::TASK_ARTIFACT,
            Self::WasLastTransitionedTo => semantic_labels::WAS_LAST_TRANSITIONED_TO,
            Self::WasLastExecutedBy => semantic_labels::WAS_LAST_EXECUTED_BY,
            Self::ScopedTo => crate::vocabulary::context_scope::SCOPED_TO,
        }
    }
}

/// Sealed marker trait for ZST edge witnesses. The set of types implementing
/// this trait is closed (defined in this file); an external crate cannot
/// invent a new edge witness.
pub trait EdgeWitness: Sealed {}

/// Compile-time witness that `Self` is a permitted primary-edge label for
/// event marker `E`. The `WRITE_SITE_RULE` is a structured doc comment that
/// surfaces in `cargo doc` and reflects the doctrinal mapping in
/// [`crate::ConversationGraphTraversal`].
///
/// Method resolution on [`crate::metamodel::writer::MetamodelWriter`] is
/// gated on this trait: `record_primary_edge::<W>` only compiles when an
/// `AllowedPrimaryEdge<E>` impl exists for `W` against the current event
/// marker `E`. Removing the impl removes the method.
pub trait AllowedPrimaryEdge<E: GraphEvent>: EdgeWitness {
    const REL: SemanticEdge;
}

// ---------------------------------------------------------------------------
// Witness ZSTs and impls. Adding an event ↔ edge mapping = adding an impl
// block here. The macro keeps boilerplate per witness to one block.
// ---------------------------------------------------------------------------

macro_rules! edge_witness {
    ($name:ident => $edge:ident; allowed_for: [$($event:path),* $(,)?]) => {
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $name;
        impl Sealed for $name {}
        impl EdgeWitness for $name {}
        $(
            impl AllowedPrimaryEdge<$event> for $name {
                const REL: SemanticEdge = SemanticEdge::$edge;
            }
        )*
    };
}

// ============================================================================
// Message events: MessageReceived / MessageSent
// ============================================================================
// Per `MAPPING_MESSAGE_RECEIVED.expected_edges = &[EDGE_WAS_RECEIVED_BY,
// EDGE_A2A_TASK_MESSAGE]` and `MAPPING_MESSAGE_SENT.expected_edges =
// &[EDGE_WAS_EMITTED_BY, EDGE_A2A_TASK_MESSAGE]`.

edge_witness!(WasReceivedByMessageProcessing => WasReceivedBy;
    allowed_for: [events::MessageReceived]);
edge_witness!(WasEmittedByMessageProcessing => WasEmittedBy;
    allowed_for: [events::MessageSent]);
edge_witness!(TaskMessageLink => A2aTaskMessage;
    allowed_for: [events::MessageReceived, events::MessageSent]);

// ============================================================================
// LLM call events: LlmCallStarted / LlmCallCompleted
// ============================================================================
// Per `MAPPING_LLM_CALL_*.expected_edges = &[EDGE_WAS_USED_BY,
// EDGE_WAS_INVOKED_BY]`. WAS_INVOKED_BY here is incident on LlmCall as the
// to-end (parent activity → LlmCall); the writer must respect direction.

edge_witness!(LlmCallUsedPrompt => WasUsedBy;
    allowed_for: [events::LlmCallStarted, events::LlmCallCompleted]);
edge_witness!(LlmCallInvokedByActivity => WasInvokedBy;
    allowed_for: [events::LlmCallStarted, events::LlmCallCompleted]);

// ============================================================================
// Tool call events: ToolCallStarted / ToolCallCompleted
// ============================================================================
// Per `MAPPING_TOOL_CALL_*.expected_edges = &[EDGE_WAS_USED_BY,
// EDGE_WAS_EXECUTED_BY]`. WAS_EXECUTED_BY is conditional today: it is
// only emitted when the writer can resolve the executing
// AgentRuntimeInstance from the surrounding scope (see
// `crate::normalizer`).

edge_witness!(ToolCallUsedArgs => WasUsedBy;
    allowed_for: [events::ToolCallStarted, events::ToolCallCompleted]);
edge_witness!(ToolCallExecutedByAgent => WasExecutedBy;
    allowed_for: [events::ToolCallStarted, events::ToolCallCompleted]);

// ============================================================================
// Task lifecycle events
// ============================================================================

edge_witness!(TaskExecutionWasCreatedBy => WasCreatedBy;
    allowed_for: [events::TaskExecutionStarted, events::TaskExecutionEnded]);
edge_witness!(TaskExecutionWasExecutedBy => WasExecutedBy;
    allowed_for: [events::TaskExecutionStarted]);
edge_witness!(TaskStateWasUpdatedBy => WasUpdatedBy;
    allowed_for: [events::TaskStatusChanged]);
edge_witness!(TaskStateWasTransitionedFrom => WasTransitionedFrom;
    allowed_for: [events::TaskStatusChanged]);
edge_witness!(ArtifactWasGeneratedBy => WasGeneratedBy;
    allowed_for: [events::TaskArtifactGenerated]);

// ============================================================================
// Agent lifecycle events
// ============================================================================

edge_witness!(AgentBootWasSpawnedBy => WasSpawnedBy;
    allowed_for: [events::AgentBooted]);
edge_witness!(AgentBootWasExecutedBy => WasExecutedBy;
    allowed_for: [events::AgentBooted]);
edge_witness!(AgentStopWasAssociatedWith => WasAssociatedWith;
    allowed_for: [events::AgentStopped]);

// ============================================================================
// Other events
// ============================================================================

edge_witness!(PromptRejectedWasUsedBy => WasUsedBy;
    allowed_for: [events::PromptRejected]);
edge_witness!(CallbackDispatchWasScheduledFrom => WasGeneratedBy;
    allowed_for: [events::CallbackDispatchContextsLinked]);

// Suppress unused-import warnings for edge constants that are only
// referenced indirectly via `SemanticEdge::as_rel_str`. Keeping the imports
// asserts at compile-time that the metamodel constants still exist.
const _METAMODEL_EDGE_REFS: &[&str] = &[
    EDGE_WAS_USED_BY,
    EDGE_WAS_RECEIVED_BY,
    EDGE_WAS_EMITTED_BY,
    EDGE_WAS_GENERATED_BY,
    EDGE_WAS_CREATED_BY,
    EDGE_WAS_EXECUTED_BY,
    EDGE_WAS_INVOKED_BY,
    EDGE_WAS_UPDATED_BY,
    EDGE_WAS_TRANSITIONED_FROM,
    EDGE_WAS_LAST_TRANSITIONED_TO,
    EDGE_WAS_LAST_EXECUTED_BY,
    EDGE_WAS_SPAWNED_BY,
    EDGE_WAS_ASSOCIATED_WITH,
    EDGE_WAS_CLASSIFIED_BY,
    EDGE_A2A_TASK_MESSAGE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_str_routes_through_vocabulary() {
        assert_eq!(SemanticEdge::WasReceivedBy.as_rel_str(), "WAS_RECEIVED_BY");
        assert_eq!(SemanticEdge::WasEmittedBy.as_rel_str(), "WAS_EMITTED_BY");
        assert_eq!(
            SemanticEdge::A2aTaskMessage.as_rel_str(),
            "A2A_TASK_MESSAGE"
        );
        assert_eq!(SemanticEdge::WasInvokedBy.as_rel_str(), "WAS_INVOKED_BY");
        assert_eq!(SemanticEdge::A2aTaskCall.as_rel_str(), "A2A_TASK_CALL");
        assert_eq!(
            SemanticEdge::A2aMessageCall.as_rel_str(),
            "A2A_MESSAGE_CALL"
        );
        assert_eq!(
            SemanticEdge::WasAssociatedWith.as_rel_str(),
            "WAS_ASSOCIATED_WITH"
        );
        assert_eq!(
            SemanticEdge::A2aTaskArtifact.as_rel_str(),
            "A2A_TASK_ARTIFACT"
        );
        assert_eq!(
            SemanticEdge::WasLastTransitionedTo.as_rel_str(),
            "WAS_LAST_TRANSITIONED_TO"
        );
        assert_eq!(
            SemanticEdge::WasLastExecutedBy.as_rel_str(),
            "WAS_LAST_EXECUTED_BY"
        );
    }

    #[test]
    fn message_received_witnesses_resolve_to_correct_edges() {
        // Compile-time check via type-level membership; the asserts confirm
        // the runtime mirror agrees.
        assert_eq!(
            <WasReceivedByMessageProcessing as AllowedPrimaryEdge<events::MessageReceived>>::REL,
            SemanticEdge::WasReceivedBy
        );
        assert_eq!(
            <TaskMessageLink as AllowedPrimaryEdge<events::MessageReceived>>::REL,
            SemanticEdge::A2aTaskMessage
        );
    }
}
