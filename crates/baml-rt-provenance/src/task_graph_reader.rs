//! [`TaskGraphReader`] — graph-only read surface for A2A tasks.
//!
//! This trait is the boundary `baml-rt-a2a` (and any other crate that
//! needs a wire-shaped Task view) consumes; the typed
//! [`crate::metamodel::query`] surface remains private to provenance and
//! is not re-exported. Implementors translate trait method calls into
//! `GraphQuery` / `EdgeProjection` reads against `prov_node` /
//! `prov_edge`; the relational-mirror tables (`a2a_task` / `a2a_message`
//! / `a2a_update`) are not consulted.
//!
//! ## Why a trait
//!
//! Two reasons it is not a free function on
//! [`crate::SurrealProvenanceStore`]:
//!
//! 1. **Boundary preservation** — keeping the surface as an
//!    `async_trait` method set lets `baml-rt-a2a` depend on the trait
//!    rather than on the concrete store, so the broadcast / session
//!    layer can be unit-tested with a mock implementor.
//! 2. **Cluster topology agnostic** — the same surface can later be
//!    backed by a remote provenance reader (e.g. across pod boundaries)
//!    without rewriting every call site.

use async_trait::async_trait;
use baml_rt_core::ids::{ActivityAnchorId, ArtifactId, ContextId, MessageId, TaskId};
use futures_util::stream::BoxStream;
use thiserror::Error;

use crate::{
    error::ProvenanceError,
    metamodel::{A2ATaskStateProps, ScopedTaskRef},
};

/// Typed reference to a `Message` graph node. Carries only the
/// node-id-equivalent identifiers a subscriber needs to hydrate via
/// [`TaskGraphReader::hydrate_batch`]; the broadcaster itself never
/// marshals wire JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub context_id: ContextId,
    pub message_id: MessageId,
}

/// Typed reference to an immutable task artifact emission. The
/// `(artifact_id, artifact_type)` pair is the stable wire lookup key
/// recovered from the anchor-keyed Artifact node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub task_id: TaskId,
    pub artifact_id: Option<ArtifactId>,
    pub artifact_type: Option<String>,
}

/// Exact replay cursor for one immutable task update.
///
/// Ordering is lexicographic on `(event_order, anchor)`. The anchor is
/// retained explicitly so replay windows remain stable even when
/// multiple facts share the same event-order bucket.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskReplayCursor {
    event_order: u64,
    anchor: ActivityAnchorId,
}

/// Construction failure for [`TaskReplayCursor`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskReplayCursorError {
    #[error("activity anchor `{0}` is not a monotonic prov-* anchor")]
    InvalidAnchor(String),
    #[error("event order {event_order} does not match anchor `{anchor}`")]
    EventOrderMismatch { event_order: u64, anchor: String },
}

impl TaskReplayCursor {
    pub fn from_anchor(anchor: ActivityAnchorId) -> Result<Self, TaskReplayCursorError> {
        let event_order = parse_event_order(&anchor)?;
        Ok(Self {
            event_order,
            anchor,
        })
    }

    pub fn try_new(
        event_order: u64,
        anchor: ActivityAnchorId,
    ) -> Result<Self, TaskReplayCursorError> {
        let parsed = parse_event_order(&anchor)?;
        if parsed != event_order {
            return Err(TaskReplayCursorError::EventOrderMismatch {
                event_order,
                anchor: anchor.as_str().to_string(),
            });
        }
        Ok(Self {
            event_order,
            anchor,
        })
    }

    pub fn event_order(&self) -> u64 {
        self.event_order
    }

    pub fn anchor(&self) -> &ActivityAnchorId {
        &self.anchor
    }
}

/// One immutable task update fact for a single `(ContextId, TaskId)`
/// stream.
///
/// `#[non_exhaustive]` keeps the variant set additive; callers at the
/// wire boundary must keep a catch-all arm so future replayable facts do
/// not silently disappear.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TaskReplayEvent {
    /// New `A2ATaskState` head — emitted on every `TaskStatusChanged`
    /// graph commit.
    StatusTransition {
        state: A2ATaskStateProps,
        cursor: TaskReplayCursor,
    },
    /// New `Artifact` node generated for the task — emitted on every
    /// `TaskArtifactGenerated` graph commit.
    ArtifactGenerated {
        artifact: ArtifactRef,
        cursor: TaskReplayCursor,
    },
    /// User → agent message accepted and persisted. Emitted on every
    /// `MessageReceived` graph commit.
    MessageReceived {
        message: MessageRef,
        cursor: TaskReplayCursor,
    },
    /// Agent → user message persisted. Emitted on every `MessageSent`
    /// graph commit.
    MessageSent {
        message: MessageRef,
        cursor: TaskReplayCursor,
    },
}

impl TaskReplayEvent {
    pub fn cursor(&self) -> &TaskReplayCursor {
        match self {
            Self::StatusTransition { cursor, .. }
            | Self::ArtifactGenerated { cursor, .. }
            | Self::MessageReceived { cursor, .. }
            | Self::MessageSent { cursor, .. } => cursor,
        }
    }

    pub fn anchor(&self) -> &ActivityAnchorId {
        self.cursor().anchor()
    }
}

/// Compatibility alias used by the live broadcaster / A2A session layer.
pub type TaskUpdateFrame = TaskReplayEvent;

/// Graph-derived projection of one A2A task — the wire-shaped view
/// reconstructed by [`TaskGraphReader::hydrate`] from `prov_node` /
/// `prov_edge` reads (no `a2a_task` / `a2a_message` involvement).
///
/// `metadata` and `extra` are deliberately not mirrored; the graph task
/// view only exposes typed provenance-backed fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydratedTask {
    pub context_id: ContextId,
    pub task_id: TaskId,
    pub status: Option<A2ATaskStateProps>,
    pub messages: Vec<MessageRef>,
    pub artifacts: Vec<ArtifactRef>,
}

/// Failures surfaced by [`TaskGraphReader::replay_since`] inside the
/// stream's `Item` slot. The outer `Result` only fails when the stream
/// itself cannot be opened.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The replay backend failed to materialise the next frame
    /// (typically a transport / decode error from the graph store).
    #[error("replay source failure: {0}")]
    Source(String),
    #[error(transparent)]
    InvalidCursor(#[from] TaskReplayCursorError),
}

impl From<ProvenanceError> for ReplayError {
    fn from(err: ProvenanceError) -> Self {
        Self::Source(err.to_string())
    }
}

/// Read surface for graph-derived task views. The methods below are the
/// only entry points downstream crates use to build wire `Task` JSON;
/// the typed [`crate::metamodel::query`] surface remains private to
/// provenance.
#[async_trait]
pub trait TaskGraphReader: Send + Sync {
    /// Resolve `(ctx, task_id)` into a [`ScopedTaskRef`] iff the Task
    /// node exists on disk **and** is `SCOPED_TO` `ctx`. Returns
    /// `Ok(None)` for non-existent or cross-context tasks.
    async fn resolve_scoped(
        &self,
        ctx: &ContextId,
        task_id: &TaskId,
    ) -> Result<Option<ScopedTaskRef>, ProvenanceError>;

    /// Resolve a Task by its id alone. Used by the wire-level
    /// `tasks.get` / `tasks.subscribe` JSON-RPC handlers, which receive
    /// only `{ id }` and have no `ContextId` to scope against.
    /// Implementors look up the Task node by id, traverse its
    /// `SCOPED_TO` edge to find the owning context, and return a
    /// [`ScopedTaskRef`] anchored to the discovered context. Returns
    /// `Ok(None)` if the Task node is absent or the `SCOPED_TO` edge
    /// is missing (a graph-write regression).
    async fn resolve_by_task_id(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<ScopedTaskRef>, ProvenanceError>;

    /// Hydrate one `ScopedTaskRef` into a wire-shaped task view.
    /// `history_cap` truncates the message list to the most recent N
    /// entries (`None` returns the full history).
    async fn hydrate(
        &self,
        scoped: ScopedTaskRef,
        history_cap: Option<usize>,
    ) -> Result<HydratedTask, ProvenanceError>;

    /// Batch hydrate. The implementor must execute a fixed number of
    /// queries regardless of `scoped.len()` — the head-pointer edges
    /// (`WAS_LAST_TRANSITIONED_TO`) collapse the latest-state stage to
    /// a single indexed-edge IN-list lookup; message and artifact
    /// fan-out follow the same `from_id IN $ids` pattern.
    async fn hydrate_batch(
        &self,
        scoped: &[ScopedTaskRef],
        history_cap: Option<usize>,
    ) -> Result<Vec<HydratedTask>, ProvenanceError>;

    /// Every Task `SCOPED_TO` `ctx`, in order of `prov_time DESC` (most
    /// recent first). Cheap — returns only `ScopedTaskRef` envelopes;
    /// follow with [`Self::hydrate_batch`] when full views are needed.
    async fn list_scoped(&self, ctx: &ContextId) -> Result<Vec<ScopedTaskRef>, ProvenanceError>;

    /// Every Task in the graph, with each entry's owning context
    /// resolved via the `SCOPED_TO` edge. Used by the wire-level
    /// `tasks.list` JSON-RPC handler when the caller did not specify a
    /// `contextId` filter. Implementors must order by `prov_time DESC`.
    async fn list_all(&self) -> Result<Vec<ScopedTaskRef>, ProvenanceError>;

    /// Most recently created Task in `ctx`, or `Ok(None)` if the
    /// context has no tasks.
    async fn latest_in_context(
        &self,
        ctx: &ContextId,
    ) -> Result<Option<ScopedTaskRef>, ProvenanceError>;

    /// Latest [`A2ATaskStateProps`] for `scoped`. Reads through the
    /// `WAS_LAST_TRANSITIONED_TO` head-pointer — single indexed edge
    /// hop, no `ORDER BY`, no `LIMIT`.
    async fn latest_state(
        &self,
        scoped: ScopedTaskRef,
    ) -> Result<Option<A2ATaskStateProps>, ProvenanceError>;

    /// Stream every [`TaskReplayEvent`] for `scoped` whose replay cursor
    /// is strictly greater than `since`. `since = None` means "from the
    /// start of the task". Implementors must preserve lexicographic
    /// ordering on `(event_order, anchor)`.
    async fn replay_since(
        &self,
        scoped: ScopedTaskRef,
        since: Option<TaskReplayCursor>,
    ) -> Result<BoxStream<'_, Result<TaskReplayEvent, ReplayError>>, ProvenanceError>;
}

fn parse_event_order(anchor: &ActivityAnchorId) -> Result<u64, TaskReplayCursorError> {
    anchor
        .as_str()
        .strip_prefix("prov-")
        .and_then(|raw| raw.parse::<u64>().ok())
        .ok_or_else(|| TaskReplayCursorError::InvalidAnchor(anchor.as_str().to_string()))
}
