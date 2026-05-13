//! `MetamodelWriter<E>` — the typed write-side facade.
//!
//! Consumed by the Message arms of [`crate::normalizer`].
//! `record_primary_edge::<W>` is bounded on `W: AllowedPrimaryEdge<E>`,
//! so an event arm cannot emit an edge the metamodel does not bless for
//! that event.
//!
//! Other normalizer arms still use the legacy free `insert_*` helpers
//! side-by-side; their migration to this facade is incremental.

use std::marker::PhantomData;

use crate::{
    metamodel::{edges::AllowedPrimaryEdge, events::GraphEvent},
    types::{ProvActivityId, ProvEntityId},
};

/// Endpoint of a primary edge — either an entity, an activity, or an agent.
/// Mirrors the shape of [`crate::types::ProvNodeRef`] but is typed by the
/// node label of the witness's expected target.
#[derive(Debug, Clone)]
pub enum NodeEndpoint {
    Entity(ProvEntityId),
    Activity(ProvActivityId),
}

/// Thin owned record of an edge the typed writer would emit. The
/// normalizer arm translates these into `ProvDocument::insert_*` calls;
/// the indirection lets the writer be tested in isolation.
#[derive(Debug, Clone)]
pub struct TypedPrimaryEdge {
    pub from: NodeEndpoint,
    pub to: NodeEndpoint,
    pub rel: crate::metamodel::edges::SemanticEdge,
}

/// Typed facade over one normalizer arm's contribution. Parameterised by the
/// event marker `E: GraphEvent`, which gates which edge witnesses can be
/// passed to [`Self::record_primary_edge`] and which `RequiredProps` shape
/// is accepted by [`Self::commit_primary`].
#[derive(Debug)]
pub struct MetamodelWriter<E: GraphEvent> {
    edges: Vec<TypedPrimaryEdge>,
    _e: PhantomData<E>,
}

impl<E: GraphEvent> MetamodelWriter<E> {
    pub fn new() -> Self {
        Self {
            edges: Vec::new(),
            _e: PhantomData,
        }
    }

    /// Record a primary edge from `from` to `to` of the type encoded by
    /// witness `W`. The bound `W: AllowedPrimaryEdge<E>` is the heart of
    /// write-side enforcement — only witnesses blessed by the metamodel for
    /// event `E` satisfy this bound.
    pub fn record_primary_edge<W>(&mut self, _witness: W, from: NodeEndpoint, to: NodeEndpoint)
    where
        W: AllowedPrimaryEdge<E>,
    {
        self.edges.push(TypedPrimaryEdge {
            from,
            to,
            rel: <W as AllowedPrimaryEdge<E>>::REL,
        });
    }

    /// Borrow the recorded edges — used by the normalizer arm to translate
    /// into `ProvDocument` insertions, and by tests to assert behaviour.
    pub fn edges(&self) -> &[TypedPrimaryEdge] {
        &self.edges
    }

    /// Consume the writer, returning the recorded edges and the primary
    /// `RequiredProps` payload. The MessageReceived normalizer arm
    /// turns this into a `ProvDocument` insertion plus the
    /// corresponding `Used` / `WasGeneratedBy` PROV rows.
    pub fn commit_primary(self, props: E::RequiredProps) -> CommittedTypedFragment<E> {
        CommittedTypedFragment {
            edges: self.edges,
            props,
            _e: PhantomData,
        }
    }
}

impl<E: GraphEvent> Default for MetamodelWriter<E> {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of [`MetamodelWriter::commit_primary`]. Carries the typed
/// `RequiredProps` payload alongside the recorded primary edges so the
/// normalizer arm can apply both atomically.
#[derive(Debug)]
pub struct CommittedTypedFragment<E: GraphEvent> {
    pub edges: Vec<TypedPrimaryEdge>,
    pub props: E::RequiredProps,
    _e: PhantomData<E>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        metamodel::{
            edges::{SemanticEdge, TaskMessageLink, WasReceivedByMessageProcessing},
            events::{MessageDirection, MessageReceived, MessageReceivedProps},
            node_ids::MessageNodeId,
        },
        types::{ProvActivityId, ProvEntityId},
    };

    fn entity(id: &str) -> NodeEndpoint {
        NodeEndpoint::Entity(ProvEntityId::test_only(id.to_string()))
    }
    fn activity(id: &str) -> NodeEndpoint {
        NodeEndpoint::Activity(ProvActivityId::test_only(id.to_string()))
    }

    #[test]
    fn writer_records_blessed_edges() {
        let mut w = MetamodelWriter::<MessageReceived>::new();
        w.record_primary_edge(
            WasReceivedByMessageProcessing,
            activity("processing:1"),
            entity("msg:1"),
        );
        w.record_primary_edge(TaskMessageLink, entity("task:1"), entity("msg:1"));
        let committed = w.commit_primary(MessageReceivedProps {
            message_id: MessageNodeId::new("msg:1"),
            role: "ROLE_USER".into(),
            content: vec!["hi".into()],
            direction: MessageDirection::Inbound,
        });
        assert_eq!(committed.edges.len(), 2);
        assert!(
            committed
                .edges
                .iter()
                .any(|e| e.rel == SemanticEdge::WasReceivedBy)
        );
        assert!(
            committed
                .edges
                .iter()
                .any(|e| e.rel == SemanticEdge::A2aTaskMessage)
        );
    }
}
