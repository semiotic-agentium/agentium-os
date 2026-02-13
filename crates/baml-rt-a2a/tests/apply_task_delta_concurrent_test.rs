//! I2: Concurrency test for apply_task_delta. Ensures no interleaving violations:
//! concurrent apply_task_delta calls for the same task result in a valid store state.

mod common;

use baml_rt_a2a::a2a_store::{TaskChunkApplier, TaskStore};
use baml_rt_a2a::a2a_types::TaskState;
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn apply_task_delta_concurrent_same_task_valid_final_state() {
    let store: Arc<Mutex<TaskStore>> = Arc::new(Mutex::new(TaskStore::new()));
    let task_id = TaskId::from_external(ExternalId::new("concurrent-task-1"));
    let context_id = ContextId::new(1, 1);

    // Chunk 1: create task and set SUBMITTED (task shell + first status)
    let chunk1_task = common::minimal_task(
        &task_id,
        &context_id,
        Some(common::task_status("TASK_STATE_SUBMITTED")),
    );
    let _ = (*store)
        .apply_task_delta(Some(chunk1_task), None, None, None)
        .await
        .unwrap();
    // Chunk 2: move to WORKING (task required when status_update present)
    let chunk2_task = common::minimal_task(&task_id, &context_id, None);
    let _ = (*store)
        .apply_task_delta(
            Some(chunk2_task),
            None,
            Some(baml_rt_a2a::a2a_types::TaskStatusUpdateEvent {
                context_id: Some(context_id.clone()),
                task_id: Some(task_id.clone()),
                status: Some(common::task_status("TASK_STATE_WORKING")),
                metadata: None,
                extra: HashMap::new(),
            }),
            None,
        )
        .await
        .unwrap();

    // Run two apply_task_delta concurrently: one moves to INPUT_REQUIRED, one to COMPLETED.
    // Both are valid from WORKING; one will win. Final state must be valid (one status).
    let store2 = store.clone();
    let task_id2 = task_id.clone();
    let context_id2 = context_id.clone();
    let chunk_h1 = common::minimal_task(&task_id2, &context_id2, None);
    let h1 = tokio::spawn(async move {
        (*store2)
            .apply_task_delta(
                Some(chunk_h1),
                None,
                Some(baml_rt_a2a::a2a_types::TaskStatusUpdateEvent {
                    context_id: Some(context_id2.clone()),
                    task_id: Some(task_id2.clone()),
                    status: Some(common::task_status("TASK_STATE_INPUT_REQUIRED")),
                    metadata: None,
                    extra: HashMap::new(),
                }),
                None,
            )
            .await
    });
    let store3 = store.clone();
    let task_id3 = task_id.clone();
    let context_id3 = context_id.clone();
    let chunk_h2 = common::minimal_task(&task_id3, &context_id3, None);
    let h2 = tokio::spawn(async move {
        (*store3)
            .apply_task_delta(
                Some(chunk_h2),
                None,
                Some(baml_rt_a2a::a2a_types::TaskStatusUpdateEvent {
                    context_id: Some(context_id3.clone()),
                    task_id: Some(task_id3.clone()),
                    status: Some(common::task_status("TASK_STATE_COMPLETED")),
                    metadata: None,
                    extra: HashMap::new(),
                }),
                None,
            )
            .await
    });

    let _r1 = h1.await.unwrap();
    let _r2 = h2.await.unwrap();

    let st = store.lock().await;
    let task = st.get(task_id.as_str(), None).expect("task exists");
    let state_str = task
        .status
        .as_ref()
        .and_then(|s| s.state.as_ref())
        .and_then(|st| match st {
            TaskState::String(s) => Some(s.as_str()),
            _ => None,
        });
    assert!(
        state_str.is_some(),
        "I2: final state must be set (no interleaving corruption)"
    );
    let s = state_str.unwrap();
    assert!(
        s == "TASK_STATE_INPUT_REQUIRED" || s == "TASK_STATE_COMPLETED",
        "I2: final state must be one of the two applied (got {})",
        s
    );
}
