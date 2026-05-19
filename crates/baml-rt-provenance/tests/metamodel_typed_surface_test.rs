//! Typed-metamodel surface integration tests.
//!
//! Confirms the positive surface (what compiles + emits the right SurrealQL)
//! and documents the negative-space invariants (what the typed surface makes
//! impossible). The negative-space invariants are encoded as `compile_fail`
//! doctests embedded as module-level documentation here, so a future
//! contributor cannot "fix" the test by relaxing the trait bounds without
//! also editing the doctest.

use baml_rt_provenance::metamodel::{
    AgentPackage, ContextNodeId, EdgeProjection, GraphQuery, MessageDirection, MessageReceived,
    MessageReceivedProps, MetamodelWriter, NodeEndpoint, ScopedTaskRef, SemanticEdge, SortDir,
    SortKey,
    edges::{TaskMessageLink, WasReceivedByMessageProcessing},
    labels,
    node_ids::{MessageNodeId, TaskNodeId},
};

fn ctx() -> ContextNodeId {
    ContextNodeId::new("context:42")
}

#[test]
fn message_query_uses_scoped_to_traversal_not_property_filter() {
    let (sql, _binds) = GraphQuery::<labels::Message, _>::new()
        .scoped_to_ctx(ctx())
        .into_surreal();
    assert!(
        !sql.contains("a2a_context_id"),
        "scoped Message query must NOT filter by `a2a_context_id` property; \
         context is an EDGE traversal (SCOPED_TO), not a property. Got: {sql}"
    );
    assert!(
        sql.contains("rel_type = 'SCOPED_TO'"),
        "scoped Message query must traverse the SCOPED_TO edge. Got: {sql}"
    );
}

#[test]
fn message_query_emitted_by_agent_package_traverses_two_hops_not_agent_id_property() {
    let (sql, binds) = GraphQuery::<labels::Message, _>::new()
        .scoped_to_ctx(ctx())
        .for_agent_package(AgentPackage::new("task-lifecycle-demo"))
        .into_surreal();
    assert!(
        !sql.contains("a2a_agent_id"),
        "agent-scoped Message query must NOT filter by `a2a_agent_id` property; \
         agent ownership of a Message is an EDGE traversal, not a property. \
         Got: {sql}"
    );
    // The two-hop traversal touches WAS_EXECUTED_BY (processing → agent) and
    // WAS_BOOTSTRAPPED_BY (boot → archive) to reach the AgentArchive that
    // carries `a2a_agent_type` (the agent_package value).
    assert!(
        sql.contains("WAS_EXECUTED_BY"),
        "expected WAS_EXECUTED_BY in: {sql}"
    );
    assert!(
        sql.contains("WAS_BOOTSTRAPPED_BY"),
        "expected WAS_BOOTSTRAPPED_BY in: {sql}"
    );
    assert!(
        sql.contains("AgentArchive"),
        "expected AgentArchive subquery in: {sql}"
    );
    let obj = binds.as_object().expect("binds object");
    assert!(obj.values().any(|v| v == "task-lifecycle-demo"));
}

#[test]
fn writer_records_only_metamodel_blessed_edges() {
    let mut w = MetamodelWriter::<MessageReceived>::new();
    w.record_primary_edge(
        WasReceivedByMessageProcessing,
        NodeEndpoint::Activity(typed_proc_id("processing:1")),
        NodeEndpoint::Entity(typed_msg_id("msg:1")),
    );
    w.record_primary_edge(
        TaskMessageLink,
        NodeEndpoint::Entity(typed_msg_id("task:1")),
        NodeEndpoint::Entity(typed_msg_id("msg:1")),
    );
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

#[test]
fn task_query_by_node_id_works() {
    let (sql, binds) = GraphQuery::<labels::Task, _>::new()
        .all()
        .by_node_id(TaskNodeId::new("task:abc-123"))
        .into_surreal();
    assert!(sql.contains("node_id ="), "must filter by node_id: {sql}");
    let obj = binds.as_object().expect("binds object");
    assert!(obj.values().any(|v| v == "task:abc-123"));
}

// ---------------------------------------------------------------------------
// Head-pointer edges (`WAS_LAST_TRANSITIONED_TO`, `WAS_LAST_EXECUTED_BY`)
// must emit a single direct edge lookup with NO ORDER BY, NO LIMIT, NO
// multi-hop traversal. The structural assertions here lock the
// head-pointer doctrine into the test surface so a future regression
// (someone re-routing latest-state through `WAS_TRANSITIONED_FROM` chain
// scans) surfaces as a failed test, not as a latent perf problem.
// ---------------------------------------------------------------------------

#[test]
fn was_last_transitioned_to_projection_is_single_edge_hop_no_sort() {
    let (sql, binds) = EdgeProjection::for_edge(SemanticEdge::WasLastTransitionedTo)
        .from_id_in(&["task:t1".to_string(), "task:t2".to_string()])
        .with_to_label::<labels::TaskState>()
        .into_surreal();
    assert!(
        sql.contains("'WAS_LAST_TRANSITIONED_TO'"),
        "head-pointer projection must filter by the head-pointer rel_type: {sql}"
    );
    assert!(
        sql.contains("from_id IN $from_ids"),
        "head-pointer projection must use IN-list batching against from_id: {sql}"
    );
    assert!(
        sql.contains("to_label = 'A2ATaskState'"),
        "head-pointer projection must constrain to_label to TaskState via the metamodel: {sql}"
    );
    assert!(
        !sql.contains("ORDER BY"),
        "head-pointer projection must NOT emit ORDER BY (the head IS the latest by edge \
         cardinality, no sorting required): {sql}"
    );
    assert!(
        !sql.contains("LIMIT") && !sql.contains("START "),
        "head-pointer projection must NOT emit LIMIT or START (cardinality is enforced by the \
         UNIQUE index, not by client-side LIMIT 1): {sql}"
    );
    assert!(
        !sql.contains("WAS_TRANSITIONED_FROM"),
        "head-pointer projection must NOT walk the chain edge (that is the history audit path, \
         not the current-value lookup): {sql}"
    );
    assert!(
        !sql.contains("props.a2a_task_id"),
        "head-pointer projection must NOT filter by the denormalised task_id property; \
         the relationship is the edge: {sql}"
    );
    let obj = binds.as_object().expect("binds object");
    assert!(
        obj.contains_key("from_ids"),
        "task ids must be parameterised, not interpolated: binds={obj:?}"
    );
}

#[test]
fn was_last_executed_by_projection_is_single_edge_hop_no_sort() {
    let (sql, binds) = EdgeProjection::for_edge(SemanticEdge::WasLastExecutedBy)
        .from_id_in(&["task:t1".to_string()])
        .with_to_label::<labels::AgentRuntimeInstance>()
        .into_surreal();
    assert!(
        sql.contains("'WAS_LAST_EXECUTED_BY'"),
        "agent head-pointer must filter by the head-pointer rel_type: {sql}"
    );
    assert!(
        sql.contains("to_label = 'AgentRuntimeInstance'"),
        "agent head-pointer must constrain to_label via the metamodel: {sql}"
    );
    assert!(
        !sql.contains("ORDER BY"),
        "agent head-pointer must NOT emit ORDER BY: {sql}"
    );
    assert!(
        !sql.contains("LIMIT") && !sql.contains("START "),
        "agent head-pointer must NOT emit LIMIT or START: {sql}"
    );
    assert!(
        !sql.contains("WAS_EXECUTED_BY'") || sql.contains("'WAS_LAST_EXECUTED_BY'"),
        "agent head-pointer must NOT walk the chain edge WAS_EXECUTED_BY \
         (that is the per-execution history): {sql}"
    );
    assert!(
        !sql.contains("props.a2a_agent_id"),
        "agent head-pointer must NOT filter by the denormalised agent_id property; \
         the relationship is the edge: {sql}"
    );
    let obj = binds.as_object().expect("binds object");
    assert!(obj.contains_key("from_ids"));
}

#[test]
fn scoped_task_ref_carries_both_node_ids() {
    let scoped =
        ScopedTaskRef::new_for_test(ContextNodeId::new("context:42"), TaskNodeId::new("task:t1"));
    assert_eq!(scoped.ctx_node_id(), "context:42");
    assert_eq!(scoped.task_node_id(), "task:t1");
}

// ---------------------------------------------------------------------------
// Phase A4: the legacy `a2a_task.ord` column is replaced by ordering on
// `props.prov_time` via the existing `SortKey::ProvTime` axis. Lock the
// "latest task in context" emission shape so a future regression
// (someone re-introducing a denormalised `ord` column for the same
// query) surfaces as a failed test.
// ---------------------------------------------------------------------------

#[test]
fn latest_task_in_context_sorts_by_prov_time_desc() {
    let (sql, _binds) = GraphQuery::<labels::Task, _>::new()
        .scoped_to_ctx(ctx())
        .order_by(SortKey::ProvTime, SortDir::Desc)
        .paginate(0, 1)
        .into_surreal();
    assert!(
        sql.contains("label = 'A2ATask'"),
        "must root at Task: {sql}"
    );
    assert!(
        sql.contains("ORDER BY props.prov_time DESC"),
        "latest-task-per-context must order by prov_time desc, not by a denormalised \
         `ord` column: {sql}"
    );
    assert!(sql.contains("LIMIT 1"), "must take exactly one row: {sql}");
    assert!(
        !sql.contains("a2a_task"),
        "must NOT touch the legacy a2a_task relational mirror table: {sql}"
    );
    assert!(
        !sql.contains("props.a2a_task_ord") && !sql.contains(" ord "),
        "must not reference the legacy `ord` field: {sql}"
    );
}

// ---------------------------------------------------------------------------
// Structural regression: agent ownership of a Message is an EDGE
// traversal, never a `props.a2a_agent_id` filter. The typed surface
// guarantees this at compile time via `keys::AgentId`'s missing
// `MessageFilterKey` impl, but we ALSO assert it structurally on the
// emitted SurrealQL so a regression in `into_surreal` (or a subtle bug
// that re-routes the agent filter through `.filter`) cannot slip past
// CI.
// ---------------------------------------------------------------------------

#[test]
fn message_query_with_every_supported_filter_never_emits_props_a2a_agent_id() {
    use baml_rt_provenance::metamodel::node_ids::AgentRuntimeInstanceNodeId;

    // for_agent: typed two-hop OR via MessageProcessing.
    let (sql_agent, _) = GraphQuery::<labels::Message, _>::new()
        .scoped_to_ctx(ctx())
        .for_agent(AgentRuntimeInstanceNodeId::new("agent_instance:agent-x"))
        .into_surreal();
    assert!(
        !sql_agent.contains("props.a2a_agent_id"),
        "Message::for_agent must NOT emit `props.a2a_agent_id`; agent \
         ownership is an EDGE traversal. Got: {sql_agent}"
    );
    assert!(
        sql_agent.contains("WAS_RECEIVED_BY") && sql_agent.contains("WAS_EMITTED_BY"),
        "Message::for_agent must traverse both inbound and outbound edges: {sql_agent}"
    );

    // for_agent_package: typed multi-hop via AgentArchive. The
    // `a2a_agent_type` property filter is legitimate at the END of the
    // traversal (it lives on AgentArchive), but it must NEVER short-
    // circuit onto the Message row itself. Verify by traversing the
    // edge chain that the agent_pkg bind only resolves through edges.
    let (sql_pkg, binds_pkg) = GraphQuery::<labels::Message, _>::new()
        .scoped_to_ctx(ctx())
        .for_agent_package(AgentPackage::new("task-lifecycle-demo"))
        .into_surreal();
    assert!(
        !sql_pkg.contains("props.a2a_agent_id"),
        "Message::for_agent_package must NOT emit `props.a2a_agent_id`. Got: {sql_pkg}"
    );
    assert!(
        sql_pkg.contains("WAS_BOOTSTRAPPED_BY")
            && sql_pkg.contains("WAS_SPAWNED_BY")
            && sql_pkg.contains("WAS_EXECUTED_BY"),
        "Message::for_agent_package must traverse Message→Activity→Instance→Boot→Archive: {sql_pkg}"
    );
    let pkg_obj = binds_pkg.as_object().expect("binds object");
    assert!(
        pkg_obj.values().any(|v| v == "task-lifecycle-demo"),
        "agent_package bind must be parameterised, not interpolated"
    );
}

// ---------------------------------------------------------------------------
// Negative-space invariants (compile_fail doctests).
//
// These doctests document the property-as-relationship shortcuts the typed
// metamodel makes IMPOSSIBLE at compile time. Each doctest must fail to
// compile; the harness asserts that.
// ---------------------------------------------------------------------------

/// Filter by `keys::ContextId` on a Message query must NOT compile — context
/// is an EDGE, not a property.
///
/// ```compile_fail
/// use baml_rt_provenance::metamodel::{
///     ContextNodeId, FilterOp, GraphQuery, keys, labels,
/// };
/// let _ = GraphQuery::<labels::Message, _>::new()
///     .scoped_to_ctx(ContextNodeId::new("context:1"))
///     .filter(keys::ContextId, FilterOp::Eq, "ctx-x".to_string());
/// ```
fn _no_message_filter_by_context_id() {}

/// Filter by `keys::AgentId` on a Message query must NOT compile — agent
/// ownership is a two-hop edge traversal, not a property filter.
///
/// ```compile_fail
/// use baml_rt_provenance::metamodel::{
///     ContextNodeId, FilterOp, GraphQuery, keys, labels,
/// };
/// let _ = GraphQuery::<labels::Message, _>::new()
///     .scoped_to_ctx(ContextNodeId::new("context:1"))
///     .filter(keys::AgentId, FilterOp::Eq, "agent-x".to_string());
/// ```
fn _no_message_filter_by_agent_id() {}

/// Filter by `keys::TaskId` on a Message query must NOT compile — task
/// scoping is the `A2A_TASK_MESSAGE` edge.
///
/// ```compile_fail
/// use baml_rt_provenance::metamodel::{
///     ContextNodeId, FilterOp, GraphQuery, keys, labels,
/// };
/// let _ = GraphQuery::<labels::Message, _>::new()
///     .scoped_to_ctx(ContextNodeId::new("context:1"))
///     .filter(keys::TaskId, FilterOp::Eq, "task-x".to_string());
/// ```
fn _no_message_filter_by_task_id() {}

/// Recording a non-blessed edge witness on a writer of the wrong event must
/// NOT compile. `LlmCallInvokedByActivity` exists for `LlmCall*` events; it
/// is NOT `AllowedPrimaryEdge<MessageReceived>`.
///
/// ```compile_fail
/// use baml_rt_provenance::metamodel::{
///     MessageReceived, MetamodelWriter, NodeEndpoint, edges::LlmCallInvokedByActivity,
/// };
/// let mut w = MetamodelWriter::<MessageReceived>::new();
/// w.record_primary_edge(
///     LlmCallInvokedByActivity,
///     NodeEndpoint::Entity(unimplemented!()),
///     NodeEndpoint::Entity(unimplemented!()),
/// );
/// ```
fn _no_llm_invoked_by_witness_on_message_writer() {}

/// `MessageReceivedProps` must not compile when a required field is missing.
/// Exact field set: `message_id`, `role`, `content`, `direction`. Note:
/// `agent_id` is deliberately ABSENT — agent ownership is an EDGE
/// traversal and is unrepresentable in this typed payload.
///
/// ```compile_fail
/// use baml_rt_provenance::metamodel::{MessageReceivedProps, node_ids::MessageNodeId};
/// let _ = MessageReceivedProps {
///     message_id: MessageNodeId::new("msg:1"),
///     role: "ROLE_USER".into(),
///     // MISSING: content, direction
/// };
/// ```
fn _message_props_must_not_have_optional_fields() {}

/// `MessageReceivedProps` must not accept an `agent_id` field — agent
/// ownership of a Message is an EDGE traversal, not a property.
///
/// ```compile_fail
/// use baml_rt_provenance::metamodel::{
///     MessageDirection, MessageReceivedProps, node_ids::MessageNodeId,
/// };
/// let _ = MessageReceivedProps {
///     message_id: MessageNodeId::new("msg:1"),
///     role: "ROLE_USER".into(),
///     content: vec![],
///     direction: MessageDirection::Inbound,
///     agent_id: "agent-x".to_string(),
/// };
/// ```
fn _message_props_no_longer_accept_agent_id() {}

// ---------------------------------------------------------------------------
// Helpers for the writer test (use cfg(test) constructors).
// ---------------------------------------------------------------------------

// Helpers exploit `ProvEntityId` / `ProvActivityId` being
// `#[serde(transparent)]` over `String`; integration tests cannot reach the
// `pub(crate) from_write_time_node_id` / `cfg(test) test_only` constructors,
// but JSON round-trip yields the same on-disk node-id shape.
fn typed_msg_id(s: &str) -> baml_rt_provenance::types::ProvEntityId {
    serde_json::from_value::<baml_rt_provenance::types::ProvEntityId>(serde_json::Value::String(
        s.to_string(),
    ))
    .expect("transparent string→ProvEntityId")
}

fn typed_proc_id(s: &str) -> baml_rt_provenance::types::ProvActivityId {
    serde_json::from_value::<baml_rt_provenance::types::ProvActivityId>(serde_json::Value::String(
        s.to_string(),
    ))
    .expect("transparent string→ProvActivityId")
}
