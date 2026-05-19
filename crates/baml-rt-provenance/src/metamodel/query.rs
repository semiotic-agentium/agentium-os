// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed graph-query DSL.
//!
//! [`GraphQuery<Subject, Scope>`] is the only legal way to construct a
//! SurrealQL query against a `Message` / `ToolCall` / `Task` /
//! `SessionStep` / `LlmCall` / `AgentStop` node from the provenance crate.
//! It enforces:
//!
//! 1. **Scope typestate.** `Unscoped` queries cannot be emitted; the caller
//!    must transition to `ScopedToContext` via [`GraphQuery::scoped_to_ctx`]
//!    or to `Unbounded` via [`GraphQuery::all`] (used only by maintenance /
//!    listing read paths that legitimately want every row).
//!
//! 2. **Subject-specific filter keys.** `.filter(key, op, val)` only accepts
//!    keys that implement the subject's filter-key trait. Crucially,
//!    [`crate::metamodel::keys::ContextId`] / `TaskId` / `AgentId` do NOT
//!    implement these traits — they are edges, not filters. A future PR
//!    cannot reintroduce `WHERE m.a2a_context_id = $ctx`.
//!
//! 3. **Composed traversals as named constructors.** Cross-node read paths
//!    (e.g., "messages emitted by an agent in a given package", "tool
//!    calls for a task") are exposed as named methods like
//!    [`GraphQuery::<labels::Message, _>::for_agent`], NOT as raw `WHERE`
//!    construction. Each named constructor encodes one canonical
//!    multi-hop path from [`crate::ConversationGraphTraversal`].
//!
//! 4. **Time / outcome / sort / pagination as typed clauses.**
//!    [`GraphQuery::with_time_range`], [`GraphQuery::with_outcome_segment`],
//!    [`GraphQuery::order_by`], [`GraphQuery::paginate`] each accept typed
//!    enums (no raw column strings) so callers cannot smuggle arbitrary
//!    SQL fragments into the WHERE / ORDER BY tail.

use std::marker::PhantomData;

use baml_rt_core::ids::ActivityAnchorId;
use serde_json::Value;

use crate::{
    metamodel::{
        edges::SemanticEdge,
        keys::FilterKey,
        labels::{self, NodeLabelTy},
        node_ids::{
            AgentPackage, AgentRuntimeInstanceNodeId, ContextNodeId, MessageNodeId,
            TaskExecutionNodeId, TaskNodeId,
        },
        sealed::Sealed,
    },
    store::ProvenanceOutcomeSegment,
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{a2a, context_scope},
};

// ---------------------------------------------------------------------------
// Scope typestate
// ---------------------------------------------------------------------------

/// Sealed marker for the scope state of a query. Implementations of
/// `into_surreal()` are bounded on `Self: Scoped` so that `Unscoped` queries
/// cannot be emitted accidentally.
pub trait ScopeState: Sealed {}

/// Marker that a [`GraphQuery`] has been transitioned to a context-scoped
/// or unbounded state and can be emitted. `Unscoped` does not implement
/// this trait.
pub trait Scoped: ScopeState {}

#[derive(Debug, Default, Clone, Copy)]
pub struct Unscoped;
impl Sealed for Unscoped {}
impl ScopeState for Unscoped {}

#[derive(Debug, Clone)]
pub struct ScopedToContext {
    ctx: ContextNodeId,
}
impl Sealed for ScopedToContext {}
impl ScopeState for ScopedToContext {}
impl Scoped for ScopedToContext {}

/// Maintenance / listing scope. Use sparingly; most reads should be
/// context-scoped.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unbounded;
impl Sealed for Unbounded {}
impl ScopeState for Unbounded {}
impl Scoped for Unbounded {}

// ---------------------------------------------------------------------------
// Subject-specific filter-key traits.
//
// These traits are sealed and the impls deliberately omit
// `keys::ContextId` / `keys::TaskId` / `keys::AgentId`. A `.filter` call
// with one of those keys against any subject is a compile error.
// ---------------------------------------------------------------------------

pub trait MessageFilterKey: FilterKey + Sealed {}
impl MessageFilterKey for crate::metamodel::keys::MessageId {}
impl MessageFilterKey for crate::metamodel::keys::Role {}
impl MessageFilterKey for crate::metamodel::keys::Direction {}

pub trait ToolCallFilterKey: FilterKey + Sealed {}
impl ToolCallFilterKey for crate::metamodel::keys::ToolName {}
impl ToolCallFilterKey for crate::metamodel::keys::ActivityOutcome {}

pub trait LlmCallFilterKey: FilterKey + Sealed {}
impl LlmCallFilterKey for crate::metamodel::keys::Model {}
impl LlmCallFilterKey for crate::metamodel::keys::Client {}
impl LlmCallFilterKey for crate::metamodel::keys::Provider {}
impl LlmCallFilterKey for crate::metamodel::keys::FunctionName {}
impl LlmCallFilterKey for crate::metamodel::keys::BamlPrompt {}
impl LlmCallFilterKey for crate::metamodel::keys::ActivityOutcome {}

pub trait TaskFilterKey: FilterKey + Sealed {}
// Tasks have very few directly filterable properties; status comes through
// the `TaskState` chain.

pub trait AgentStopFilterKey: FilterKey + Sealed {}
// AgentStop has no scalar filter keys today.

pub trait SessionStepFilterKey: FilterKey + Sealed {}
impl SessionStepFilterKey for crate::metamodel::keys::ToolName {}

// ---------------------------------------------------------------------------
// Filter operators / sort keys / segment markers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum FilterOp {
    Eq,
    NotEq,
}

impl FilterOp {
    fn as_op(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::NotEq => "!=",
        }
    }
}

/// Sealed sort-column enum. New sort columns can only be added inside the
/// metamodel — call sites cannot pass arbitrary column strings.
#[derive(Debug, Clone, Copy)]
pub enum SortKey {
    /// `props.a2a_event_order` — canonical chronological key for messages
    /// and most activities. Falls back to `0` when missing.
    EventOrder,
    /// `props.a2a_activity_anchor` — primary chronological key for
    /// activities (LlmCall, ToolCall, AgentStop, …).
    ActivityAnchor,
    /// `props.prov_time` — write timestamp on the prov_node row.
    ProvTime,
}

impl SortKey {
    fn column(self) -> &'static str {
        match self {
            Self::EventOrder => a2a::EVENT_ORDER,
            Self::ActivityAnchor => a2a::ACTIVITY_ANCHOR,
            Self::ProvTime => "prov_time",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

// ---------------------------------------------------------------------------
// Internal clause types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PropertyFilter {
    prop_key: &'static str,
    op: FilterOp,
    bind: String,
    value: Value,
}

/// A single `WHERE`-clause fragment + its bindings, built by a typed
/// constructor inside this module. Call sites cannot construct these
/// directly: the only producers are subject-typed methods on
/// [`GraphQuery`].
#[derive(Debug, Clone)]
struct OpaqueClause {
    sql: String,
    binds: Vec<(String, Value)>,
}

// ---------------------------------------------------------------------------
// GraphQuery — the typed read builder.
// ---------------------------------------------------------------------------

/// Typed graph query over a single subject node label. Construct via
/// [`GraphQuery::<Subject>::new`] and transition via
/// [`Self::scoped_to_ctx`] / [`Self::all`] before emission.
#[derive(Debug, Clone)]
pub struct GraphQuery<Subject: NodeLabelTy, S: ScopeState> {
    scope: S,
    filters: Vec<PropertyFilter>,
    /// Time-range fence on `props.a2a_event_order`. Inclusive on both ends.
    time_range: Option<(Option<u64>, Option<u64>)>,
    /// Outcome segment on `props.a2a_activity_outcome` (success / failed /
    /// both). Defaults to `Both` (no clause emitted).
    outcome_segment: Option<ProvenanceOutcomeSegment>,
    /// Pagination: `(offset, limit)`.
    page: Option<(u64, u64)>,
    /// Explicit `ORDER BY` directive. When unset, no ORDER BY is emitted.
    order: Option<(SortKey, SortDir)>,
    /// Composed multi-hop traversals (each from a typed constructor).
    opaque: Vec<OpaqueClause>,
    bind_counter: u64,
    _subject: PhantomData<Subject>,
}

impl<Subject: NodeLabelTy> GraphQuery<Subject, Unscoped> {
    pub fn new() -> Self {
        Self {
            scope: Unscoped,
            filters: Vec::new(),
            time_range: None,
            outcome_segment: None,
            page: None,
            order: None,
            opaque: Vec::new(),
            bind_counter: 0,
            _subject: PhantomData,
        }
    }

    /// Transition to a context-scoped query. Only scoped queries may be
    /// emitted to SurrealQL; this is the canonical entry-point for every
    /// read path that filters by context membership.
    ///
    /// Compiles to `SCOPED_TO` edge traversal — never to a property
    /// filter on `a2a_context_id`.
    pub fn scoped_to_ctx(self, ctx: ContextNodeId) -> GraphQuery<Subject, ScopedToContext> {
        GraphQuery {
            scope: ScopedToContext { ctx },
            filters: self.filters,
            time_range: self.time_range,
            outcome_segment: self.outcome_segment,
            page: self.page,
            order: self.order,
            opaque: self.opaque,
            bind_counter: self.bind_counter,
            _subject: PhantomData,
        }
    }

    /// Transition to an unbounded query (every node of this label). Use
    /// only for maintenance / listing read paths.
    pub fn all(self) -> GraphQuery<Subject, Unbounded> {
        GraphQuery {
            scope: Unbounded,
            filters: self.filters,
            time_range: self.time_range,
            outcome_segment: self.outcome_segment,
            page: self.page,
            order: self.order,
            opaque: self.opaque,
            bind_counter: self.bind_counter,
            _subject: PhantomData,
        }
    }
}

impl<Subject: NodeLabelTy> Default for GraphQuery<Subject, Unscoped> {
    fn default() -> Self {
        Self::new()
    }
}

// Order/limit/time/outcome helpers are available in any scope state.
impl<Subject: NodeLabelTy, S: ScopeState> GraphQuery<Subject, S> {
    /// Inclusive `event_order` time fence. Either bound may be omitted to
    /// leave that side open.
    pub fn with_time_range(mut self, from_ms: Option<u64>, to_ms: Option<u64>) -> Self {
        self.time_range = Some((from_ms, to_ms));
        self
    }

    pub fn with_outcome_segment(mut self, segment: ProvenanceOutcomeSegment) -> Self {
        self.outcome_segment = Some(segment);
        self
    }

    pub fn order_by(mut self, key: SortKey, dir: SortDir) -> Self {
        self.order = Some((key, dir));
        self
    }

    pub fn paginate(mut self, offset: u64, limit: u64) -> Self {
        self.page = Some((offset, limit));
        self
    }

    fn next_bind(&mut self, hint: &str) -> String {
        self.bind_counter = self.bind_counter.saturating_add(1);
        format!("{hint}_{}", self.bind_counter)
    }

    fn push_opaque(&mut self, sql: String, binds: Vec<(String, Value)>) {
        self.opaque.push(OpaqueClause { sql, binds });
    }
}

// ---------------------------------------------------------------------------
// Internal subquery builders (the only place inside the crate that
// interpolates `SemanticEdge::*` into SQL strings).
// ---------------------------------------------------------------------------

/// Subquery that yields AgentRuntimeInstance node IDs whose archive's
/// `a2a_agent_type` matches the bound package value. Used by every
/// `for_agent_package` traversal.
fn agent_instances_in_package_subquery(pkg_bind: &str) -> String {
    let spawned = SemanticEdge::WasSpawnedBy.as_rel_str();
    let bootstrapped = SemanticEdge::WasBootstrappedBy.as_rel_str();
    format!(
        "SELECT VALUE from_id FROM {TBL_EDGE} \
         WHERE rel_type = '{spawned}' AND to_id IN (\
            SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE rel_type = '{bootstrapped}' AND to_id IN (\
                SELECT VALUE node_id FROM {TBL_NODE} \
                WHERE label = '{archive_label}' AND props.a2a_agent_type = ${pkg_bind}\
            )\
         )",
        archive_label = labels::AgentArchive::LABEL_STR,
    )
}

/// Inner-traversal target: either a scalar bind (for `for_agent`) or a
/// subquery yielding many AgentRuntimeInstance node_ids (for
/// `for_agent_package`).
enum AgentTarget<'a> {
    /// `to_id = $bind`
    ScalarBind(&'a str),
    /// `to_id IN (<subquery>)`
    Subquery(&'a str),
}

impl<'a> AgentTarget<'a> {
    fn as_predicate(&self) -> String {
        match self {
            Self::ScalarBind(bind) => format!("to_id = ${bind}"),
            Self::Subquery(sq) => format!("to_id IN ({sq})"),
        }
    }
}

/// Two-hop OR pattern: Message ↔ MessageProcessing ↔ AgentRuntimeInstance.
/// Returns a `(received OR emitted)` predicate that matches messages
/// processed by the agent target.
/// Conversation-context export filter: keep only Message / ToolCall / SessionStep rows
/// owned by agents bootstrapped from the archive package bound as `$agent_pkg`.
pub(crate) fn conversation_node_matches_agent_package_sql(pkg_bind: &str) -> String {
    let subq = agent_instances_in_package_subquery(pkg_bind);
    let msg = message_to_agent_traversal(AgentTarget::Subquery(&subq));
    let call = call_activity_to_agent_traversal(AgentTarget::Subquery(&subq));
    format!(
        "AND ((label = 'Message' AND {msg}) OR (label IN ('ToolCall', 'SessionStep') AND {call}))"
    )
}

fn message_to_agent_traversal(target: AgentTarget<'_>) -> String {
    let received = SemanticEdge::WasReceivedBy.as_rel_str();
    let emitted = SemanticEdge::WasEmittedBy.as_rel_str();
    let executed = SemanticEdge::WasExecutedBy.as_rel_str();
    let agent_pred = target.as_predicate();
    format!(
        "(node_id IN (\
            SELECT VALUE to_id FROM {TBL_EDGE} \
            WHERE rel_type = '{received}' \
            AND from_id IN (\
                SELECT VALUE from_id FROM {TBL_EDGE} \
                WHERE rel_type = '{executed}' AND {agent_pred}\
            )\
         ) OR node_id IN (\
            SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE rel_type = '{emitted}' \
            AND to_id IN (\
                SELECT VALUE from_id FROM {TBL_EDGE} \
                WHERE rel_type = '{executed}' AND {agent_pred}\
            )\
         ))"
    )
}

/// Two-hop pattern for LlmCall / ToolCall: the call activity is reachable
/// from `AgentRuntimeInstance` via either the message-scoped
/// (`A2A_MESSAGE_CALL`) or task-scoped (`A2A_TASK_CALL`) parent activity,
/// each of which is `WAS_EXECUTED_BY` the agent. Symmetric to the
/// `MESSAGE_TO_AGENT` traversal on `Message`.
///
/// On-disk shape:
/// `(c:LlmCall|ToolCall) <-[:A2A_MESSAGE_CALL|:A2A_TASK_CALL]- (p:A2AMessageProcessing|A2ATaskExecution) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)`
///
/// Conceptually equivalent to the W3C-PROV
/// `LLM_CALL_TO_AGENT` documented on
/// [`crate::ConversationGraphTraversal::LLM_CALL_TO_AGENT`]; the
/// `A2A_*_CALL` relation labels are the persisted form of `WAS_INVOKED_BY`
/// (LlmCall) / `WAS_EXECUTED_BY` (ToolCall) per
/// `crates/baml-rt-provenance/PROV_MAPPING.md`.
fn call_activity_to_agent_traversal(target: AgentTarget<'_>) -> String {
    let message_call = SemanticEdge::A2aMessageCall.as_rel_str();
    let task_call = SemanticEdge::A2aTaskCall.as_rel_str();
    let executed = SemanticEdge::WasExecutedBy.as_rel_str();
    let agent_pred = target.as_predicate();
    format!(
        "(node_id IN (\
            SELECT VALUE to_id FROM {TBL_EDGE} \
            WHERE rel_type = '{message_call}' \
            AND from_id IN (\
                SELECT VALUE from_id FROM {TBL_EDGE} \
                WHERE rel_type = '{executed}' AND {agent_pred}\
            )\
         ) OR node_id IN (\
            SELECT VALUE to_id FROM {TBL_EDGE} \
            WHERE rel_type = '{task_call}' \
            AND from_id IN (\
                SELECT VALUE from_id FROM {TBL_EDGE} \
                WHERE rel_type = '{executed}' AND {agent_pred}\
            )\
         ))"
    )
}

/// Single-hop pattern for AgentStop: the stop activity is directly
/// `WAS_ASSOCIATED_WITH` the AgentRuntimeInstance (no `EXECUTING_AGENT`
/// role on the stop association, so the writer stores the canonical
/// PROV-O `WAS_ASSOCIATED_WITH` rel_type).
///
/// On-disk shape: `(s:AgentStop) -[:WAS_ASSOCIATED_WITH]-> (a:AgentRuntimeInstance)`.
fn agent_stop_to_agent_traversal(target: AgentTarget<'_>) -> String {
    let associated = SemanticEdge::WasAssociatedWith.as_rel_str();
    let agent_pred = target.as_predicate();
    format!(
        "node_id IN (\
            SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE rel_type = '{associated}' AND {agent_pred}\
         )"
    )
}

// ---------------------------------------------------------------------------
// Subject-specific constructors. Each subject uses its own sealed
// `*FilterKey` trait so cross-subject filters do not compile (e.g.,
// `keys::ContextId` cannot be passed as a `MessageFilterKey` — context is
// an edge, not a property).
// ---------------------------------------------------------------------------

impl<S: ScopeState> GraphQuery<labels::Message, S> {
    pub fn filter<K: MessageFilterKey>(mut self, _key: K, op: FilterOp, value: K::Value) -> Self
    where
        K::Value: Into<Value>,
    {
        let bind = self.next_bind("p");
        self.filters.push(PropertyFilter {
            prop_key: K::PROP_KEY,
            op,
            bind,
            value: value.into(),
        });
        self
    }

    /// Restrict to messages received OR emitted by a specific agent
    /// runtime instance. Encodes the canonical two-hop OR traversal:
    ///
    /// `(m:Message) <-[:WAS_RECEIVED_BY]- (p:A2AMessageProcessing) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)`
    /// OR
    /// `(m:Message) -[:WAS_EMITTED_BY]-> (p:A2AMessageProcessing) -[:WAS_EXECUTED_BY]-> (a:AgentRuntimeInstance)`
    pub fn for_agent(mut self, agent: AgentRuntimeInstanceNodeId) -> Self {
        let bind = self.next_bind("agent_node");
        let value = Value::String(agent.into_string());
        let sql = message_to_agent_traversal(AgentTarget::ScalarBind(&bind));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    /// Restrict to messages emitted/received by any agent of the given
    /// package. Equivalent to chaining
    /// [`Self::for_agent`] over every AgentRuntimeInstance bootstrapped
    /// from the matching AgentArchive.
    pub fn for_agent_package(mut self, package: AgentPackage) -> Self {
        let bind = self.next_bind("agent_pkg");
        let value = Value::String(package.into_string());
        let subq = agent_instances_in_package_subquery(&bind);
        let sql = message_to_agent_traversal(AgentTarget::Subquery(&subq));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    /// Restrict to messages owned by a specific Task (entity). Encodes
    /// the `A2A_TASK_MESSAGE` edge traversal `Task → Message`.
    pub fn for_task(mut self, task: TaskNodeId) -> Self {
        let bind = self.next_bind("task_node");
        let value = Value::String(task.into_string());
        let edge = SemanticEdge::A2aTaskMessage.as_rel_str();
        let sql = format!(
            "node_id IN (\
                SELECT VALUE to_id FROM {TBL_EDGE} \
                WHERE rel_type = '{edge}' AND from_id = ${bind}\
             )"
        );
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    /// Lookup a single Message by on-disk node id.
    pub fn by_node_id(mut self, id: MessageNodeId) -> Self {
        let bind = self.next_bind("msg_node_id");
        let value = Value::String(id.into_string());
        self.push_opaque(format!("node_id = ${bind}"), vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::ToolCall, S> {
    pub fn filter<K: ToolCallFilterKey>(mut self, _key: K, op: FilterOp, value: K::Value) -> Self
    where
        K::Value: Into<Value>,
    {
        let bind = self.next_bind("p");
        self.filters.push(PropertyFilter {
            prop_key: K::PROP_KEY,
            op,
            bind,
            value: value.into(),
        });
        self
    }

    pub fn for_agent(mut self, agent: AgentRuntimeInstanceNodeId) -> Self {
        let bind = self.next_bind("agent_node");
        let value = Value::String(agent.into_string());
        let sql = call_activity_to_agent_traversal(AgentTarget::ScalarBind(&bind));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    pub fn for_agent_package(mut self, package: AgentPackage) -> Self {
        let bind = self.next_bind("agent_pkg");
        let value = Value::String(package.into_string());
        let subq = agent_instances_in_package_subquery(&bind);
        let sql = call_activity_to_agent_traversal(AgentTarget::Subquery(&subq));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    /// Restrict to tool-calls owned by a specific Task. The
    /// `A2A_TASK_CALL` edge originates at the [`TaskExecution`]
    /// activity (not the Task entity), so callers must pass a
    /// [`TaskExecutionNodeId`].
    pub fn for_task_execution(mut self, exec: TaskExecutionNodeId) -> Self {
        let bind = self.next_bind("task_exec_node");
        let value = Value::String(exec.into_string());
        let edge = SemanticEdge::A2aTaskCall.as_rel_str();
        let sql = format!(
            "node_id IN (\
                SELECT VALUE to_id FROM {TBL_EDGE} \
                WHERE rel_type = '{edge}' AND from_id = ${bind}\
             )"
        );
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::LlmCall, S> {
    pub fn filter<K: LlmCallFilterKey>(mut self, _key: K, op: FilterOp, value: K::Value) -> Self
    where
        K::Value: Into<Value>,
    {
        let bind = self.next_bind("p");
        self.filters.push(PropertyFilter {
            prop_key: K::PROP_KEY,
            op,
            bind,
            value: value.into(),
        });
        self
    }

    pub fn for_agent(mut self, agent: AgentRuntimeInstanceNodeId) -> Self {
        let bind = self.next_bind("agent_node");
        let value = Value::String(agent.into_string());
        let sql = call_activity_to_agent_traversal(AgentTarget::ScalarBind(&bind));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    pub fn for_agent_package(mut self, package: AgentPackage) -> Self {
        let bind = self.next_bind("agent_pkg");
        let value = Value::String(package.into_string());
        let subq = agent_instances_in_package_subquery(&bind);
        let sql = call_activity_to_agent_traversal(AgentTarget::Subquery(&subq));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    pub fn for_task_execution(mut self, exec: TaskExecutionNodeId) -> Self {
        let bind = self.next_bind("task_exec_node");
        let value = Value::String(exec.into_string());
        let edge = SemanticEdge::A2aTaskCall.as_rel_str();
        let sql = format!(
            "node_id IN (\
                SELECT VALUE to_id FROM {TBL_EDGE} \
                WHERE rel_type = '{edge}' AND from_id = ${bind}\
             )"
        );
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::AgentStop, S> {
    pub fn for_agent(mut self, agent: AgentRuntimeInstanceNodeId) -> Self {
        let bind = self.next_bind("agent_node");
        let value = Value::String(agent.into_string());
        let sql = agent_stop_to_agent_traversal(AgentTarget::ScalarBind(&bind));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }

    pub fn for_agent_package(mut self, package: AgentPackage) -> Self {
        let bind = self.next_bind("agent_pkg");
        let value = Value::String(package.into_string());
        let subq = agent_instances_in_package_subquery(&bind);
        let sql = agent_stop_to_agent_traversal(AgentTarget::Subquery(&subq));
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::SessionStep, S> {
    pub fn filter<K: SessionStepFilterKey>(mut self, _key: K, op: FilterOp, value: K::Value) -> Self
    where
        K::Value: Into<Value>,
    {
        let bind = self.next_bind("p");
        self.filters.push(PropertyFilter {
            prop_key: K::PROP_KEY,
            op,
            bind,
            value: value.into(),
        });
        self
    }

    /// Restrict to session-steps owned by a specific Task.
    /// `A2A_TASK_SESSION_STEP` originates at the Task entity.
    pub fn for_task(mut self, task: TaskNodeId) -> Self {
        let bind = self.next_bind("task_node");
        let value = Value::String(task.into_string());
        let edge = SemanticEdge::A2aTaskSessionStep.as_rel_str();
        let sql = format!(
            "node_id IN (\
                SELECT VALUE to_id FROM {TBL_EDGE} \
                WHERE rel_type = '{edge}' AND from_id = ${bind}\
             )"
        );
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::Artifact, S> {
    /// Restrict to artifact emissions owned by a specific Task entity.
    pub fn for_task(mut self, task: TaskNodeId) -> Self {
        let bind = self.next_bind("task_node");
        let value = Value::String(task.into_string());
        let edge = SemanticEdge::A2aTaskArtifact.as_rel_str();
        let sql = format!(
            "node_id IN (\
                SELECT VALUE to_id FROM {TBL_EDGE} \
                WHERE rel_type = '{edge}' AND from_id = ${bind}\
             )"
        );
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::TaskState, S> {
    /// Restrict to TaskState nodes emitted by a specific TaskExecution
    /// activity.
    pub fn for_task_execution(mut self, exec: TaskExecutionNodeId) -> Self {
        let bind = self.next_bind("task_exec_node");
        let value = Value::String(exec.into_string());
        let edge = SemanticEdge::WasUpdatedBy.as_rel_str();
        let sql = format!(
            "node_id IN (\
                SELECT VALUE to_id FROM {TBL_EDGE} \
                WHERE rel_type = '{edge}' AND from_id = ${bind}\
             )"
        );
        self.push_opaque(sql, vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::Task, S> {
    /// Lookup a single Task by on-disk node id.
    pub fn by_node_id(mut self, id: TaskNodeId) -> Self {
        let bind = self.next_bind("task_node_id");
        let value = Value::String(id.into_string());
        self.push_opaque(format!("node_id = ${bind}"), vec![(bind, value)]);
        self
    }
}

impl<S: ScopeState> GraphQuery<labels::AgentRuntimeInstance, S> {
    /// Lookup an AgentRuntimeInstance by on-disk node id.
    pub fn by_node_id(mut self, id: AgentRuntimeInstanceNodeId) -> Self {
        let bind = self.next_bind("agent_node_id");
        let value = Value::String(id.into_string());
        self.push_opaque(format!("node_id = ${bind}"), vec![(bind, value)]);
        self
    }
}

// ---------------------------------------------------------------------------
// Batch by-id lookup applies to every Subject (no traversal involved).
// ---------------------------------------------------------------------------

impl<Subject: NodeLabelTy, S: ScopeState> GraphQuery<Subject, S> {
    /// Restrict the query to a fixed set of on-disk node IDs. Equivalent
    /// to `node_id IN $ids`. Used for batch hydration from a previously
    /// gathered ID list (e.g. `FailureClassification` lookups by activity
    /// fan-out). The slice is cloned into the JSON bind value, so the
    /// caller retains ownership of its `Vec<String>`.
    pub fn by_node_ids(mut self, ids: &[String]) -> Self {
        let bind = self.next_bind("node_ids");
        let value = Value::Array(ids.iter().cloned().map(Value::String).collect());
        self.push_opaque(format!("node_id IN ${bind}"), vec![(bind, value)]);
        self
    }

    /// Restrict to rows strictly after the given replay cursor using
    /// the canonical `(event_order, activity_anchor)` lexicographic
    /// ordering.
    pub fn after_event_cursor(mut self, event_order: u64, anchor: &ActivityAnchorId) -> Self {
        let order_bind = self.next_bind("cursor_order");
        let anchor_bind = self.next_bind("cursor_anchor");
        let event_order_key = SortKey::EventOrder.column().replace(':', "_");
        let anchor_key = SortKey::ActivityAnchor.column().replace(':', "_");
        let sql = format!(
            "(props.{event_order_key} > ${order_bind} OR \
              (props.{event_order_key} = ${order_bind} AND props.{anchor_key} > ${anchor_bind}))",
        );
        self.push_opaque(
            sql,
            vec![
                (order_bind, Value::Number(event_order.into())),
                (anchor_bind, Value::String(anchor.as_str().to_string())),
            ],
        );
        self
    }
}

// ---------------------------------------------------------------------------
// EdgeProjection — typed access to the prov_edge table for the rare
// read paths that need (from_id, to_id) tuples directly.
// ---------------------------------------------------------------------------

/// Typed edge-table projection. Emits
/// `SELECT from_id, to_id FROM prov_edge WHERE rel_type = '<edge>' AND ...`.
/// The `rel_type` literal is sourced from the typed [`SemanticEdge`]
/// enum — callers cannot inline a raw `WAS_*` string. Optional
/// `from_id IN $ids` / `to_label = '<L>'` filters are typed: the
/// `to_label` constructor takes a [`NodeLabelTy`] ZST so the literal
/// label string is sourced from the metamodel, not stitched in.
#[derive(Debug, Clone)]
pub struct EdgeProjection {
    edge: SemanticEdge,
    from_ids: Option<Vec<String>>,
    to_label: Option<&'static str>,
}

impl EdgeProjection {
    /// Construct an edge projection over the given semantic edge label.
    pub fn for_edge(edge: SemanticEdge) -> Self {
        Self {
            edge,
            from_ids: None,
            to_label: None,
        }
    }

    /// Restrict to edges whose `from_id` is in the given list
    /// (`from_id IN $ids`). The slice is cloned into the projection's
    /// internal state, so the caller retains ownership of its
    /// `Vec<String>`.
    pub fn from_id_in(mut self, ids: &[String]) -> Self {
        self.from_ids = Some(ids.to_vec());
        self
    }

    /// Restrict to edges whose `to_label` matches the given typed node
    /// label. Sourced from the metamodel — not a raw string.
    pub fn with_to_label<L: NodeLabelTy>(mut self) -> Self {
        self.to_label = Some(L::LABEL_STR);
        self
    }

    /// Emit the projection as SurrealQL + bindings.
    pub fn into_surreal(self) -> (String, Bindings) {
        let rel = self.edge.as_rel_str();
        let mut where_clauses = vec![format!("rel_type = '{rel}'")];
        let mut binds = serde_json::Map::new();
        if let Some(ids) = self.from_ids {
            where_clauses.push("from_id IN $from_ids".to_string());
            binds.insert(
                "from_ids".to_string(),
                Value::Array(ids.into_iter().map(Value::String).collect()),
            );
        }
        if let Some(label) = self.to_label {
            where_clauses.push(format!("to_label = '{label}'"));
        }
        let sql = format!(
            "SELECT from_id, to_id OMIT id FROM {TBL_EDGE} WHERE {}",
            where_clauses.join(" AND ")
        );
        (sql, Value::Object(binds))
    }
}

// ---------------------------------------------------------------------------
// SurrealQL emission. Only `Scoped` queries (ScopedToContext / Unbounded)
// can be emitted; `Unscoped` cannot, by typestate.
// ---------------------------------------------------------------------------

/// Bindings produced by `GraphQuery::into_surreal` — a flat JSON object the
/// caller passes to the Surreal driver.
pub type Bindings = Value;

impl<Subject: NodeLabelTy, S: Scoped + ScopeQueryEmitter> GraphQuery<Subject, S> {
    /// Emit the query as a SurrealQL string + bindings. Only callable on
    /// `Scoped` typestates, which include `ScopedToContext` and
    /// `Unbounded`.
    pub fn into_surreal(self) -> (String, Bindings) {
        let label = Subject::LABEL_STR;
        let mut binds_map = serde_json::Map::new();
        // The label clause is the only mandatory predicate (every typed
        // query is rooted at exactly one node label). All other clauses
        // are AND-ed with it.
        let mut where_clauses: Vec<String> = vec![format!("label = '{label}'")];

        // 1. Scope clause (e.g. SCOPED_TO subquery) emitted by the scope
        //    state.
        if let Some((scope_clause, scope_binds)) = self.scope.scope_where_clause() {
            where_clauses.push(scope_clause);
            for (k, v) in scope_binds {
                binds_map.insert(k, v);
            }
        }

        // 2. Property filters. The vocabulary constants encode prop keys
        //    with colons (`a2a:role`); on disk they live as
        //    underscore-delimited columns (`a2a_role`). Mirror the
        //    conversion in `crate::surreal_sql::storage_safe_props`.
        for f in &self.filters {
            let column = f.prop_key.replace(':', "_");
            where_clauses.push(format!("props.{column} {} ${}", f.op.as_op(), f.bind));
            binds_map.insert(f.bind.clone(), f.value.clone());
        }

        // 3. Time-range fence on `props.a2a_event_order`.
        if let Some((from_ms, to_ms)) = self.time_range {
            if let Some(from) = from_ms {
                let bind = format!("time_from_{}", binds_map.len());
                where_clauses.push(format!("props.a2a_event_order >= ${bind}"));
                binds_map.insert(bind, Value::Number(from.into()));
            }
            if let Some(to) = to_ms {
                let bind = format!("time_to_{}", binds_map.len());
                where_clauses.push(format!("props.a2a_event_order <= ${bind}"));
                binds_map.insert(bind, Value::Number(to.into()));
            }
        }

        // 4. Outcome segment.
        if let Some(seg) = self.outcome_segment {
            match seg {
                ProvenanceOutcomeSegment::FailedOnly => {
                    where_clauses.push(format!(
                        "props.a2a_activity_outcome = '{}'",
                        crate::vocabulary::activity_outcome::FAILURE,
                    ));
                }
                ProvenanceOutcomeSegment::SuccessfulOnly => {
                    where_clauses.push(format!(
                        "props.a2a_activity_outcome != '{}'",
                        crate::vocabulary::activity_outcome::FAILURE,
                    ));
                }
                ProvenanceOutcomeSegment::Both => {}
            }
        }

        // 5. Named (opaque-but-typed) traversal clauses.
        for clause in &self.opaque {
            where_clauses.push(clause.sql.clone());
            for (k, v) in &clause.binds {
                binds_map.insert(k.clone(), v.clone());
            }
        }

        // 6. Compose query.
        let where_sql = format!(" WHERE {}", where_clauses.join(" AND "));
        let order_sql = match self.order {
            Some((key, dir)) => {
                // Same colon-to-underscore convention used for property
                // filters (`storage_safe_props`).
                let column = key.column().replace(':', "_");
                format!(" ORDER BY props.{column} {}", dir.as_str())
            }
            None => String::new(),
        };
        let (limit_sql, offset_sql) = match self.page {
            Some((offset, limit)) => {
                let limit = format!(" LIMIT {limit}");
                let offset = if offset > 0 {
                    format!(" START {offset}")
                } else {
                    String::new()
                };
                (limit, offset)
            }
            None => (String::new(), String::new()),
        };

        let sql = format!("SELECT * FROM {TBL_NODE}{where_sql}{order_sql}{limit_sql}{offset_sql}");
        (sql, Value::Object(binds_map))
    }
}

/// Internal helper trait that lets each `Scope` typestate produce its own
/// `WHERE` fragment (SCOPED_TO subquery, no-op for `Unbounded`, etc.).
/// Sealed so external crates cannot extend the scope set.
pub trait ScopeQueryEmitter: Sealed {
    fn scope_where_clause(&self) -> Option<(String, Vec<(String, Value)>)>;
}

impl ScopeQueryEmitter for ScopedToContext {
    fn scope_where_clause(&self) -> Option<(String, Vec<(String, Value)>)> {
        // SCOPED_TO traversal: this node is the *from* of the SCOPED_TO
        // edge, pointing at the Context node identified by `self.ctx`.
        // (`prov_edge.from_id` / `prov_edge.to_id` are the persisted
        // columns; `in.node_id` / `out.node_id` is graph-traversal
        // notation that does NOT match this schema.)
        let bind = "ctx_node_id".to_string();
        let clause = format!(
            "node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
             WHERE rel_type = '{scoped}' AND to_id = ${bind})",
            scoped = context_scope::SCOPED_TO,
        );
        let binds = vec![(bind, Value::String(self.ctx.0.clone()))];
        Some((clause, binds))
    }
}

impl ScopeQueryEmitter for Unbounded {
    fn scope_where_clause(&self) -> Option<(String, Vec<(String, Value)>)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metamodel::keys;

    fn ctx() -> ContextNodeId {
        ContextNodeId::new("context:42")
    }

    #[test]
    fn scoped_message_query_emits_scoped_to_traversal_not_property_filter() {
        let (sql, _binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .into_surreal();
        assert!(
            !sql.contains("a2a_context_id"),
            "scoped Message query must traverse SCOPED_TO, not filter by property: {sql}"
        );
        assert!(
            sql.contains("rel_type = 'SCOPED_TO'"),
            "expected SCOPED_TO traversal in: {sql}"
        );
        assert!(sql.contains("label = 'Message'"));
        // Schema sanity: scope traversal uses real columns, not graph
        // pseudo-columns.
        assert!(
            sql.contains("from_id") && sql.contains("to_id"),
            "scope clause must use prov_edge from_id/to_id columns: {sql}"
        );
        assert!(!sql.contains("in.node_id") && !sql.contains("out.node_id"));
    }

    #[test]
    fn scoped_message_query_with_role_filter_compiles_and_binds() {
        let (sql, binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .filter(keys::Role, FilterOp::Eq, "ROLE_USER".into())
            .into_surreal();
        assert!(
            sql.contains("props.a2a_role"),
            "Role filter should land on the property column: {sql}"
        );
        let obj = binds.as_object().expect("binds object");
        assert!(obj.values().any(|v| v == "ROLE_USER"));
    }

    #[test]
    fn for_agent_package_emits_two_hop_traversal() {
        let (sql, binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .for_agent_package(AgentPackage::new("task-lifecycle-demo"))
            .into_surreal();
        assert!(
            sql.contains("WAS_EXECUTED_BY"),
            "must traverse WAS_EXECUTED_BY: {sql}"
        );
        assert!(
            sql.contains("WAS_BOOTSTRAPPED_BY"),
            "must traverse WAS_BOOTSTRAPPED_BY for archive package: {sql}"
        );
        assert!(
            sql.contains("AgentArchive"),
            "must reach AgentArchive for agent_package projection: {sql}"
        );
        assert!(
            sql.contains("WAS_RECEIVED_BY") && sql.contains("WAS_EMITTED_BY"),
            "Message agent traversal must OR over received and emitted: {sql}"
        );
        let obj = binds.as_object().expect("binds object");
        assert!(obj.values().any(|v| v == "task-lifecycle-demo"));
    }

    #[test]
    fn message_for_agent_emits_or_pattern() {
        let (sql, _binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .for_agent(AgentRuntimeInstanceNodeId::new("agent_instance:abc"))
            .into_surreal();
        assert!(sql.contains("WAS_RECEIVED_BY"));
        assert!(sql.contains("WAS_EMITTED_BY"));
        assert!(sql.contains("WAS_EXECUTED_BY"));
    }

    #[test]
    fn llm_call_for_agent_uses_canonical_two_hop_via_call_edges() {
        let (sql, binds) = GraphQuery::<labels::LlmCall, _>::new()
            .scoped_to_ctx(ctx())
            .for_agent(AgentRuntimeInstanceNodeId::new("agent_instance:xyz"))
            .into_surreal();
        assert!(
            sql.contains("WAS_EXECUTED_BY"),
            "outer hop reaches AgentRuntimeInstance via WAS_EXECUTED_BY: {sql}"
        );
        assert!(
            sql.contains("A2A_MESSAGE_CALL"),
            "two-hop traversal walks A2A_MESSAGE_CALL for message-scoped calls: {sql}"
        );
        assert!(
            sql.contains("A2A_TASK_CALL"),
            "two-hop traversal walks A2A_TASK_CALL for task-scoped calls: {sql}"
        );
        assert!(
            !sql.contains("WAS_RECEIVED_BY") && !sql.contains("WAS_EMITTED_BY"),
            "LlmCall agent filter must NOT use Message-only edges: {sql}"
        );
        let obj = binds.as_object().expect("binds");
        assert!(obj.values().any(|v| v == "agent_instance:xyz"));
    }

    #[test]
    fn tool_call_for_task_execution_uses_a2a_task_call() {
        let (sql, binds) = GraphQuery::<labels::ToolCall, _>::new()
            .scoped_to_ctx(ctx())
            .for_task_execution(TaskExecutionNodeId::new("task_execution_t1"))
            .into_surreal();
        assert!(sql.contains("A2A_TASK_CALL"));
        assert!(sql.contains("from_id = $task_exec_node"));
        let obj = binds.as_object().expect("binds");
        assert!(obj.values().any(|v| v == "task_execution_t1"));
    }

    #[test]
    fn message_for_task_uses_a2a_task_message() {
        let (sql, binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .for_task(TaskNodeId::new("task:t1"))
            .into_surreal();
        assert!(sql.contains("A2A_TASK_MESSAGE"));
        let obj = binds.as_object().expect("binds");
        assert!(obj.values().any(|v| v == "task:t1"));
    }

    #[test]
    fn task_lookup_by_node_id_uses_node_id_clause() {
        let (sql, binds) = GraphQuery::<labels::Task, _>::new()
            .all()
            .by_node_id(TaskNodeId::new("task:abc-123"))
            .into_surreal();
        assert!(sql.contains("node_id ="), "must filter by node_id: {sql}");
        let obj = binds.as_object().expect("binds object");
        assert!(obj.values().any(|v| v == "task:abc-123"));
    }

    #[test]
    fn time_range_emits_event_order_bounds() {
        let (sql, binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .with_time_range(Some(100), Some(200))
            .into_surreal();
        assert!(sql.contains("props.a2a_event_order >="));
        assert!(sql.contains("props.a2a_event_order <="));
        let obj = binds.as_object().expect("binds object");
        assert!(obj.values().any(|v| v == 100));
        assert!(obj.values().any(|v| v == 200));
    }

    #[test]
    fn outcome_segment_failed_only_filters_outcome_eq_failure() {
        let (sql, _binds) = GraphQuery::<labels::LlmCall, _>::new()
            .scoped_to_ctx(ctx())
            .with_outcome_segment(ProvenanceOutcomeSegment::FailedOnly)
            .into_surreal();
        assert!(sql.contains("props.a2a_activity_outcome ="));
    }

    #[test]
    fn paginate_emits_limit_and_start() {
        let (sql, _binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .paginate(20, 10)
            .into_surreal();
        assert!(sql.contains("LIMIT 10"));
        assert!(sql.contains("START 20"));
    }

    #[test]
    fn order_by_event_order_desc_emits_typed_column() {
        let (sql, _binds) = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ctx())
            .order_by(SortKey::EventOrder, SortDir::Desc)
            .into_surreal();
        assert!(sql.contains("ORDER BY props.a2a_event_order DESC"));
    }
}
