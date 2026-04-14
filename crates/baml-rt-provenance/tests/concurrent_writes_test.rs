//! Regression coverage for the SurrealDB MVCC retry loop in
//! [`SurrealProvenanceStore::run_event_write_plan`].
//!
//! N concurrent writers all UPSERT the same shared graph records (agent runtime
//! instance, context entity, runner runtime instance) — without retry, SurrealDB
//! returns `Transaction conflict` for the loser of each concurrent pair, which
//! used to silently roll back the whole txn and look like a successful write.
//! This test fires N parallel `add_event` calls and asserts every one returns
//! `Ok`. Without the retry loop, the run reliably reports `Conflict` errors.
//!
//! ```bash
//! cargo test -p baml-rt-provenance --test concurrent_writes_test
//! ```
use std::sync::Arc;

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{ProvEvent, ProvenanceWriter, SurrealStoreBuilder};

#[tokio::test]
async fn parallel_task_execution_started_writes_succeed_under_mvcc_contention() {
    // 8 parallel writers > 6 retry budget headroom, so we exercise the loop
    // without making it the dominant cost; if a future change weakens retry,
    // this test fails reliably (verified by reverting the retry loop locally).
    const PARALLEL_WRITERS: usize = 8;

    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated in-memory store");

    let context_id = ContextId::new(7777, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

    // Each writer targets a unique task_id but the SAME context + agent, so the
    // task_entity / task_execution_activity nodes are unique per writer while
    // the context_entity / agent_runtime_instance / runner_runtime_instance
    // nodes are shared — that's where MVCC conflicts fire.
    let mut handles = Vec::with_capacity(PARALLEL_WRITERS);
    for i in 0..PARALLEL_WRITERS {
        let store = Arc::clone(&store);
        let context_id = context_id.clone();
        let agent_id = agent_id.clone();
        handles.push(tokio::spawn(async move {
            let task_id = TaskId::from_external(ExternalId::new(format!("contention-task-{i}")));
            store
                .add_event(ProvEvent::task_execution_started(
                    context_id, task_id, agent_id,
                ))
                .await
        }));
    }

    let mut failures: Vec<String> = Vec::new();
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await.expect("writer task panicked") {
            Ok(()) => {}
            Err(e) => failures.push(format!("writer[{i}]: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "all {PARALLEL_WRITERS} concurrent writers must succeed under MVCC retry; failures: {failures:?}"
    );
}
