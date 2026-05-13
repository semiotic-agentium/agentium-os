//! Newtype wrappers for typed node-id values used by [`crate::metamodel::query`].
//!
//! The wrappers exist to ensure that, e.g., `GraphQuery::scoped_to_ctx` can
//! only be called with a `ContextNodeId` (the persisted node identifier of a
//! `Context`), not a raw `&str` that might happen to be the agent-visible
//! `context_id` (which is a separate, narrower identifier shape).
//!
//! These newtypes are deliberately minimal: each carries a single owned
//! `String` corresponding to the on-disk `prov_node.node_id` column. The
//! `for_*_id` constructors encapsulate the on-disk encoding and route
//! through `crate::id_semantics` so the wire-id → node-id transform exists
//! in exactly one place.

use baml_rt_core::ids::{AgentId, ContextId, MessageId, TaskId};

/// On-disk identifier for a `Context` node (`prov_node.node_id`).
///
/// Conventionally encoded as `"context:<context_id>"` by the normalizer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextNodeId(pub(crate) String);

impl ContextNodeId {
    /// Construct from a raw on-disk identifier. Callers in tests / migrations
    /// may need to manufacture these; production code should prefer
    /// [`Self::for_context_id`].
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Canonical constructor from a wire `ContextId`. Encapsulates the
    /// `"context:<context_id>"` encoding via
    /// [`crate::id_semantics::context_entity_id_string`].
    pub fn for_context_id(ctx: &ContextId) -> Self {
        Self(crate::id_semantics::context_entity_id_string(ctx.as_str()))
    }

    /// View the on-disk `node_id` as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume self into the owned `node_id` string.
    pub fn into_string(self) -> String {
        self.0
    }
}

/// On-disk identifier for a `Message` node.
///
/// Encoded as `"message:<context_id>:<message_id>"` by the normalizer
/// (Message entities are scoped to their context to disambiguate identical
/// `MessageId` strings reused across contexts).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageNodeId(pub(crate) String);

impl MessageNodeId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Canonical constructor from a wire `(ContextId, MessageId)` pair.
    /// Mirrors the encoding used by `crate::normalizer::message_entity_id`.
    pub fn for_message_id(ctx: &ContextId, msg: &MessageId) -> Self {
        Self(format!("message:{}:{}", ctx.as_str(), msg.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// On-disk identifier for an `AgentRuntimeInstance` node. The typed
/// `AgentRuntimeInstance` projection in
/// [`crate::surreal_store::ops_query::ProvenanceOpsQuery`] keys agent
/// identity off this newtype rather than off a denormalised
/// `props.a2a_agent_id` filter on each Message row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentRuntimeInstanceNodeId(pub(crate) String);

impl AgentRuntimeInstanceNodeId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Canonical constructor from a wire `AgentId`. Mirrors the
    /// `from_parts("agent_instance", [agent_id])` encoding used by
    /// [`crate::id_semantics::AgentRuntimeInstanceId`].
    pub fn for_agent_id(agent: &AgentId) -> Self {
        Self(format!("agent_instance:{}", agent.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// On-disk identifier for an `A2ATask` node (the entity, not the activity).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskNodeId(pub(crate) String);

impl TaskNodeId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Canonical constructor from a wire `TaskId`. Encapsulates the
    /// `"task:<task_id>"` encoding via
    /// [`crate::id_semantics::task_entity_id_string_raw`].
    pub fn for_task_id(task: &TaskId) -> Self {
        Self(crate::id_semantics::task_entity_id_string_raw(
            task.as_str(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// On-disk identifier for an `A2ATaskExecution` ACTIVITY node (distinct
/// from [`TaskNodeId`] which is the Task ENTITY). The `A2A_TASK_CALL` edge
/// originates at the TaskExecution activity, not at the Task entity, so
/// queries that filter LlmCall / ToolCall by their owning task must use
/// this newtype.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskExecutionNodeId(pub(crate) String);

impl TaskExecutionNodeId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Canonical constructor from a wire `TaskId`. Mirrors
    /// [`crate::id_semantics::task_execution_activity_id_string`].
    pub fn for_task_id(task: &TaskId) -> Self {
        Self(crate::id_semantics::task_execution_activity_id_string(
            task.as_str(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Newtype for an agent package identifier (e.g. `"task-lifecycle-demo"`),
/// the value carried by the `a2a:agent_type` attribute on
/// `AgentRuntimeInstance` / `AgentArchive`. Filtering by package is a typed
/// operation that traverses the agent two-hop chain (see
/// [`crate::ConversationGraphTraversal::AGENT_TO_ARCHIVE`]); the newtype
/// prevents the caller from confusing it with `agent_id`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentPackage(pub String);

impl AgentPackage {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Typestate witness that a `Task` node has been verified to (a) exist on
/// disk and (b) be linked to a specific `Context` via the `SCOPED_TO`
/// edge. All hydration / replay APIs that operate on a single task take
/// `ScopedTaskRef` rather than raw `(ContextId, TaskId)` so a caller
/// cannot accidentally hydrate a task that lives in a different context
/// (cross-context forgery becomes structurally unrepresentable).
///
/// # Construction
///
/// The only legal production constructor is
/// [`crate::TaskGraphReader::resolve_scoped`], which performs the graph
/// existence + scope proof and returns `Some(ScopedTaskRef)` only when
/// both checks succeed. The `cfg(test)` `new_for_test` constructor exists
/// to support unit tests that bypass the full graph round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopedTaskRef {
    ctx: ContextNodeId,
    task: TaskNodeId,
}

impl ScopedTaskRef {
    /// Trusted constructor — invoked only by code that has already proved
    /// the (ctx, task) pair via a `SCOPED_TO` edge lookup. Restricted to
    /// the crate so external code must go through
    /// `TaskGraphReader::resolve_scoped` or another typed proof-producing
    /// path inside the crate.
    pub(crate) fn new_proven(ctx: ContextNodeId, task: TaskNodeId) -> Self {
        Self { ctx, task }
    }

    /// Test-only constructor. Bypasses the graph existence + scope
    /// proof; **do not use in production code paths** — production code
    /// must construct `ScopedTaskRef` exclusively through
    /// [`crate::TaskGraphReader::resolve_scoped`] so the typestate
    /// invariant (Task exists AND is `SCOPED_TO` the Context) is
    /// preserved. This constructor exists because integration tests
    /// outside the crate's `src/` cannot see `pub(crate)` items and
    /// cannot reach the trusted `new_proven` constructor.
    pub fn new_for_test(ctx: ContextNodeId, task: TaskNodeId) -> Self {
        Self { ctx, task }
    }

    pub fn ctx(&self) -> &ContextNodeId {
        &self.ctx
    }

    pub fn task(&self) -> &TaskNodeId {
        &self.task
    }

    /// Borrow the on-disk task node id as a `&str`.
    pub fn task_node_id(&self) -> &str {
        self.task.as_str()
    }

    /// Borrow the on-disk context node id as a `&str`.
    pub fn ctx_node_id(&self) -> &str {
        self.ctx.as_str()
    }
}
