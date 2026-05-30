// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed surface drives `ops_query` SQL emission.
//!
//! Each `ProvenanceOpsResource` in `surreal_store/ops_query.rs` builds
//! its WHERE clause via per-Subject [`GraphQuery`] composition (see the
//! `build_messages_query` / `build_llm_query` / `build_tool_query` /
//! `build_lifecycle_query` free functions). This integration test
//! mirrors that composition through the public typed surface and
//! asserts two structural invariants on the emitted SurrealQL:
//!
//! 1. **Positive**: the typed-traversal markers (`SCOPED_TO`,
//!    `WAS_EXECUTED_BY`, `WAS_ASSOCIATED_WITH`, `A2A_TASK_MESSAGE`,
//!    `A2A_TASK_CALL`, `A2A_MESSAGE_CALL`, `WAS_RECEIVED_BY` /
//!    `WAS_EMITTED_BY` for the Message OR-pattern) appear in the right
//!    places.
//! 2. **Negative**: agent / context / task identity never leaks into the
//!    property filter slot (`props.a2a_agent_id`, `props.a2a_context_id`,
//!    `props.a2a_task_id`) — those identities are EDGES, not properties,
//!    and the typed surface forbids the property short-circuit at
//!    compile time via the missing `*FilterKey` trait impls on
//!    [`crate::metamodel::keys`]. We verify the structural emission too,
//!    defending against a future regression in
//!    [`GraphQuery::into_surreal`] or in the per-resource builders.
//!
//! The test does NOT touch SurrealDB; the `into_surreal()` output is
//! compared as a string. Live-DB regression coverage lives in
//! `tests/message_agent_traversal_test.rs`.

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId};
use baml_rt_id::UuidId;
use baml_rt_provenance::metamodel::{
    AgentPackage, ContextNodeId, FilterOp, GraphQuery, TaskExecutionNodeId, TaskNodeId, keys,
    labels, node_ids::AgentRuntimeInstanceNodeId,
};
use serde_json::Value;

const AGENT_UUID: &str = "11111111-2222-3333-4444-555555555555";

fn ctx_node() -> ContextNodeId {
    ContextNodeId::for_context_id(&ContextId::from("ctx-test"))
}

fn agent_node() -> AgentRuntimeInstanceNodeId {
    let uuid = UuidId::parse_str(AGENT_UUID).expect("valid uuid");
    AgentRuntimeInstanceNodeId::for_agent_id(&AgentId::from_uuid(uuid))
}

fn task_id() -> TaskId {
    TaskId::from_external(ExternalId::new("task-test"))
}

fn task_node() -> TaskNodeId {
    TaskNodeId::for_task_id(&task_id())
}

fn task_exec_node() -> TaskExecutionNodeId {
    TaskExecutionNodeId::for_task_id(&task_id())
}

/// Assert that no identity-as-property filter (`props.a2a_agent_id`,
/// `props.a2a_context_id`, `props.a2a_task_id`) leaks into the SQL.
/// These identities are modelled as EDGES (`WAS_EXECUTED_BY`, `SCOPED_TO`,
/// `A2A_TASK_*`); the typed surface makes property-side filtering
/// unrepresentable at compile time, and this assertion is the structural
/// counterpart that catches a regression in `into_surreal` or in the
/// per-resource builders.
fn assert_no_identity_property_filters(sql: &str, ctx: &str) {
    assert!(
        !sql.contains("props.a2a_agent_id"),
        "{ctx}: must not emit `props.a2a_agent_id`; agent ownership is an EDGE. Got: {sql}"
    );
    assert!(
        !sql.contains("props.a2a_context_id"),
        "{ctx}: must not emit `props.a2a_context_id`; context is an EDGE (SCOPED_TO). Got: {sql}"
    );
    assert!(
        !sql.contains("props.a2a_task_id"),
        "{ctx}: must not emit `props.a2a_task_id`; task is an EDGE (A2A_TASK_*). Got: {sql}"
    );
}

// ---------------------------------------------------------------------------
// Messages resource (mirrors `build_messages_query`).
// ---------------------------------------------------------------------------

#[test]
fn messages_query_with_full_filter_set_emits_typed_traversals() {
    let (sql, binds) = GraphQuery::<labels::Message, _>::new()
        .scoped_to_ctx(ctx_node())
        .for_agent(agent_node())
        .for_agent_package(AgentPackage::new("task-lifecycle-demo"))
        .for_task(task_node())
        .into_surreal();

    // Positive structural invariants.
    assert!(
        sql.contains("label = 'Message'"),
        "Message subject label must root the query: {sql}"
    );
    assert!(
        sql.contains("rel_type = 'SCOPED_TO'"),
        "context scope traversed via SCOPED_TO edge: {sql}"
    );
    assert!(
        sql.contains("WAS_RECEIVED_BY") && sql.contains("WAS_EMITTED_BY"),
        "Message agent ownership uses OR over received/emitted MessageProcessing edges: {sql}"
    );
    assert!(
        sql.contains("WAS_EXECUTED_BY"),
        "Message agent traversal reaches AgentRuntimeInstance via WAS_EXECUTED_BY: {sql}"
    );
    assert!(
        sql.contains("WAS_BOOTSTRAPPED_BY"),
        "agent_package traversal must reach AgentArchive via WAS_BOOTSTRAPPED_BY: {sql}"
    );
    assert!(
        sql.contains("A2A_TASK_MESSAGE"),
        "Message::for_task uses the A2A_TASK_MESSAGE derived edge: {sql}"
    );

    // Negative structural invariants.
    assert_no_identity_property_filters(&sql, "Messages");

    // Bind sanity: every typed value reaches the bind map under a
    // distinct key.
    let obj = binds.as_object().expect("binds object");
    assert!(obj.values().any(|v| v == "context:ctx-test"));
    let expected_agent_node = format!("agent_instance:{AGENT_UUID}");
    assert!(obj.values().any(|v| v == expected_agent_node.as_str()));
    assert!(obj.values().any(|v| v == "task-lifecycle-demo"));
    assert!(obj.values().any(|v| v == "task:task-test"));
}

#[test]
fn messages_query_unscoped_unbounded_still_avoids_identity_filters() {
    let (sql, _) = GraphQuery::<labels::Message, _>::new()
        .all()
        .for_agent(agent_node())
        .into_surreal();
    assert!(
        !sql.contains("rel_type = 'SCOPED_TO'"),
        "Unbounded query should NOT emit SCOPED_TO traversal: {sql}"
    );
    assert_no_identity_property_filters(&sql, "Messages (unbounded)");
}

// ---------------------------------------------------------------------------
// LlmCalls / Aggregates resource (mirrors `build_llm_query`).
// ---------------------------------------------------------------------------

#[test]
fn llm_query_with_full_filter_set_emits_typed_traversals() {
    let (sql, binds) = GraphQuery::<labels::LlmCall, _>::new()
        .scoped_to_ctx(ctx_node())
        .for_agent(agent_node())
        .for_agent_package(AgentPackage::new("task-lifecycle-demo"))
        .for_task_execution(task_exec_node())
        .filter(keys::Provider, FilterOp::Eq, "openai".to_string())
        .filter(keys::Model, FilterOp::Eq, "gpt-4o".to_string())
        .into_surreal();

    assert!(sql.contains("label = 'LlmCall'"));
    assert!(sql.contains("rel_type = 'SCOPED_TO'"));
    assert!(
        sql.contains("WAS_EXECUTED_BY"),
        "LlmCall agent traversal reaches AgentRuntimeInstance via WAS_EXECUTED_BY: {sql}"
    );
    assert!(
        sql.contains("A2A_MESSAGE_CALL"),
        "LlmCall::for_agent two-hop traversal walks A2A_MESSAGE_CALL for message-scoped calls: {sql}"
    );
    assert!(
        sql.contains("A2A_TASK_CALL"),
        "LlmCall::for_agent / for_task_execution use A2A_TASK_CALL: {sql}"
    );
    assert!(
        !sql.contains("WAS_RECEIVED_BY") && !sql.contains("WAS_EMITTED_BY"),
        "LlmCall agent filter MUST NOT use Message-only edges: {sql}"
    );
    assert!(
        sql.contains("props.a2a_client = $"),
        "Provider key maps to `a2a_client` column: {sql}"
    );
    assert!(
        sql.contains("props.a2a_model = $"),
        "Model key maps to `a2a_model` column: {sql}"
    );

    assert_no_identity_property_filters(&sql, "LlmCalls");

    let obj = binds.as_object().expect("binds");
    assert!(obj.values().any(|v| v == "openai"));
    assert!(obj.values().any(|v| v == "gpt-4o"));
    assert!(obj.values().any(|v| v == "task_execution_task-test"));
}

// ---------------------------------------------------------------------------
// ToolCalls resource (mirrors `build_tool_query`).
// ---------------------------------------------------------------------------

#[test]
fn tool_query_with_full_filter_set_emits_typed_traversals() {
    let (sql, binds) = GraphQuery::<labels::ToolCall, _>::new()
        .scoped_to_ctx(ctx_node())
        .for_agent(agent_node())
        .for_task_execution(task_exec_node())
        .filter(keys::ToolName, FilterOp::Eq, "calculator".to_string())
        .into_surreal();

    assert!(sql.contains("label = 'ToolCall'"));
    assert!(sql.contains("rel_type = 'SCOPED_TO'"));
    assert!(
        sql.contains("WAS_EXECUTED_BY"),
        "ToolCall agent traversal reaches AgentRuntimeInstance via WAS_EXECUTED_BY: {sql}"
    );
    assert!(
        sql.contains("A2A_MESSAGE_CALL"),
        "ToolCall::for_agent two-hop traversal walks A2A_MESSAGE_CALL for message-scoped calls: {sql}"
    );
    assert!(
        sql.contains("A2A_TASK_CALL"),
        "ToolCall::for_agent / for_task_execution use A2A_TASK_CALL: {sql}"
    );
    assert!(sql.contains("props.a2a_tool_name = $"));

    assert_no_identity_property_filters(&sql, "ToolCalls");

    let obj = binds.as_object().expect("binds");
    assert!(obj.values().any(|v| v == "calculator"));
}

// ---------------------------------------------------------------------------
// LifecycleEvents (AgentStop) resource (mirrors `build_lifecycle_query`).
// ---------------------------------------------------------------------------

#[test]
fn lifecycle_query_with_agent_filters_emits_executed_by_traversal() {
    let (sql, _) = GraphQuery::<labels::AgentStop, _>::new()
        .scoped_to_ctx(ctx_node())
        .for_agent(agent_node())
        .into_surreal();

    assert!(sql.contains("label = 'AgentStop'"));
    assert!(sql.contains("rel_type = 'SCOPED_TO'"));
    assert!(
        sql.contains("WAS_ASSOCIATED_WITH"),
        "AgentStop agent filter uses single-hop WAS_ASSOCIATED_WITH (no EXECUTING_AGENT role on the stop association): {sql}"
    );
    assert_no_identity_property_filters(&sql, "LifecycleEvents");
}

// ---------------------------------------------------------------------------
// Edge-table projection (mirrors `load_failure_classification_for_activity_ids`).
// ---------------------------------------------------------------------------

#[test]
fn edge_projection_was_classified_by_uses_typed_label_constants() {
    use baml_rt_provenance::metamodel::{EdgeProjection, SemanticEdge};

    let activity_ids: Vec<String> = vec!["llm_call:1".into(), "tool_call:2".into()];
    let (sql, binds) = EdgeProjection::for_edge(SemanticEdge::WasClassifiedBy)
        .from_id_in(&activity_ids)
        .with_to_label::<labels::FailureClassification>()
        .into_surreal();

    assert!(
        sql.contains("rel_type = 'WAS_CLASSIFIED_BY'"),
        "edge projection sources rel_type from typed SemanticEdge: {sql}"
    );
    assert!(
        sql.contains("from_id IN $from_ids"),
        "edge projection binds from_ids parametrically: {sql}"
    );
    assert!(
        sql.contains("to_label = 'FailureClassification'"),
        "edge projection's to_label sourced from labels::FailureClassification: {sql}"
    );

    let obj = binds.as_object().expect("binds");
    let from_ids = obj.get("from_ids").expect("from_ids bind");
    assert!(matches!(from_ids, Value::Array(_)));
}

// ---------------------------------------------------------------------------
// Catch-all: emitted SurrealQL must NEVER contain a literal raw activity-
// outcome value as a string equality with the wrong column. The outcome
// segment goes through the typed `with_outcome_segment` API.
// ---------------------------------------------------------------------------

#[test]
fn outcome_segment_failed_only_uses_canonical_failure_value() {
    use baml_rt_provenance::store::ProvenanceOutcomeSegment;

    let (sql, _) = GraphQuery::<labels::LlmCall, _>::new()
        .scoped_to_ctx(ctx_node())
        .with_outcome_segment(ProvenanceOutcomeSegment::FailedOnly)
        .into_surreal();
    assert!(
        sql.contains("props.a2a_activity_outcome = 'Failed'"),
        "FailedOnly segment must compare against the canonical 'Failed' literal: {sql}"
    );
}
