//! Cardinality invariant for head-pointer edges
//! (`WAS_LAST_TRANSITIONED_TO`, `WAS_LAST_EXECUTED_BY`).
//!
//! SurrealDB v3 does not support partial / WHERE-filtered UNIQUE indexes,
//! so an unconditional `(rel_type, from_id) UNIQUE` index would break the
//! existing fan-out edges (`A2A_TASK_MESSAGE`, `A2A_TASK_ARTIFACT`, ...)
//! that legitimately share a `from_id`. The cardinality-one invariant is
//! therefore enforced procedurally by
//! `surreal_write_batch::push_head_pointer_repoint`, which emits
//! `DELETE prov_edge WHERE from_id = ? AND rel_type = ?` followed by an
//! `UPSERT` for the new head, both inside the same `BEGIN..COMMIT`
//! transaction as the rest of the event's writes.
//!
//! This test fires the writer-doctrine path:
//!
//! 1. Multiple `TaskStatusChanged` events for the same Task. Asserts
//!    exactly one `WAS_LAST_TRANSITIONED_TO` edge survives, pointing at
//!    the `TaskState` of the most recent transition. Asserts the
//!    `WAS_TRANSITIONED_FROM` chain edges are still intact (the
//!    head-pointer does not erase provenance).
//!
//! 2. Multiple `TaskExecutionStarted` events for the same Task with
//!    different agents. Asserts exactly one `WAS_LAST_EXECUTED_BY` edge
//!    survives, pointing at the `AgentRuntimeInstance` of the most
//!    recent execution.
//!
//! ```bash
//! cargo test -p baml-rt-provenance --test head_pointer_cardinality_test
//! ```

use std::sync::Arc;

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{
    ProvEvent, ProvenanceWriter, SurrealStoreBuilder,
    metamodel::{EdgeProjection, SemanticEdge},
};
use serde_json::Value;

const SUBMITTED: &str = "STATE_SUBMITTED";
const WORKING: &str = "STATE_WORKING";
const COMPLETED: &str = "STATE_COMPLETED";

async fn select_edges_via_typed_projection(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    edge: SemanticEdge,
    from_node: &str,
) -> Vec<Value> {
    let from_ids = vec![from_node.to_string()];
    let (sql, binds) = EdgeProjection::for_edge(edge)
        .from_id_in(&from_ids)
        .into_surreal();
    let mut q = store.db().query(sql);
    if let Some(obj) = binds.as_object() {
        for (k, v) in obj {
            q = q.bind((k.clone(), v.clone()));
        }
    }
    let mut response = q.await.expect("typed edge projection executes");
    let rows: Vec<Value> = response.take(0).expect("take rows");
    rows
}

fn task_node_id(task_id: &TaskId) -> String {
    baml_rt_provenance::task_entity_id_string(task_id)
}

fn task_state_node_id(task_id: &TaskId, state: &str) -> String {
    // Mirrors `id_semantics::task_state_entity_id_string`, which is not
    // re-exported. Format documented at
    // crates/baml-rt-provenance/src/id_semantics.rs::TaskStateEntityId.
    format!("task_state:{}:{}", task_id.as_str(), state)
}

fn agent_runtime_instance_node_id(agent_id: &AgentId) -> String {
    format!("agent_instance:{}", agent_id.as_str())
}

#[tokio::test]
async fn was_last_transitioned_to_keeps_exactly_one_edge_after_multiple_transitions() {
    let store: Arc<baml_rt_provenance::SurrealProvenanceStore> =
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build isolated in-memory store");

    let context_id = ContextId::new(91101, 1);
    let task_id = TaskId::from_external(ExternalId::new("hp-task-status"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap());

    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started writes");

    // None -> SUBMITTED -> WORKING -> COMPLETED.
    for (old, new) in [
        (None, Some(SUBMITTED.to_string())),
        (Some(SUBMITTED.to_string()), Some(WORKING.to_string())),
        (Some(WORKING.to_string()), Some(COMPLETED.to_string())),
    ] {
        store
            .add_event(ProvEvent::task_status_changed(
                context_id.clone(),
                task_id.clone(),
                old,
                new,
            ))
            .await
            .expect("task_status_changed writes");
    }

    let task_node = task_node_id(&task_id);
    let head_rows =
        select_edges_via_typed_projection(&store, SemanticEdge::WasLastTransitionedTo, &task_node)
            .await;

    assert_eq!(
        head_rows.len(),
        1,
        "WAS_LAST_TRANSITIONED_TO must have cardinality 1 per Task; got rows = {head_rows:?}"
    );
    let to_id = head_rows[0]
        .get("to_id")
        .and_then(Value::as_str)
        .expect("head-pointer row carries to_id");
    assert_eq!(
        to_id,
        task_state_node_id(&task_id, COMPLETED),
        "WAS_LAST_TRANSITIONED_TO must point at the most recent TaskState"
    );

    // Sanity: the `WAS_TRANSITIONED_FROM` chain edges still exist between
    // intermediate states. Two transitions => two chain edges.
    let chain_count: i64 = store
        .db()
        .query(format!(
            "SELECT count() AS c FROM prov_edge WHERE rel_type = '{}' GROUP ALL",
            baml_rt_provenance::EDGE_WAS_TRANSITIONED_FROM
        ))
        .await
        .and_then(|mut r| r.take::<Vec<Value>>(0))
        .ok()
        .and_then(|rows| rows.first()?.get("c")?.as_i64())
        .expect("chain count query");
    assert_eq!(
        chain_count, 2,
        "head-pointer doctrine must preserve `WAS_TRANSITIONED_FROM` chain edges (transitions: SUBMITTED→WORKING, WORKING→COMPLETED)"
    );
}

#[tokio::test]
async fn was_last_executed_by_keeps_exactly_one_edge_after_re_execution() {
    let store: Arc<baml_rt_provenance::SurrealProvenanceStore> =
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build isolated in-memory store");

    let context_id = ContextId::new(91102, 1);
    let task_id = TaskId::from_external(ExternalId::new("hp-task-exec"));
    let agent_a =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap());
    let agent_b =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000b2").unwrap());

    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_a.clone(),
        ))
        .await
        .expect("first task_execution_started writes");

    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_b.clone(),
        ))
        .await
        .expect("second task_execution_started writes");

    let task_node = task_node_id(&task_id);
    let head_rows =
        select_edges_via_typed_projection(&store, SemanticEdge::WasLastExecutedBy, &task_node)
            .await;

    assert_eq!(
        head_rows.len(),
        1,
        "WAS_LAST_EXECUTED_BY must have cardinality 1 per Task; got rows = {head_rows:?}"
    );
    let to_id = head_rows[0]
        .get("to_id")
        .and_then(Value::as_str)
        .expect("head-pointer row carries to_id");
    assert_eq!(
        to_id,
        agent_runtime_instance_node_id(&agent_b),
        "WAS_LAST_EXECUTED_BY must point at the most recently-started agent"
    );
}
