#![recursion_limit = "256"]
//! I2: concurrent chunk application on the graph-backed task store.

mod common;

use std::{collections::HashMap, sync::Arc};

use baml_rt_a2a::{
    a2a_store::{TaskChunkApplier, TaskRepository},
    a2a_types::{StreamResponse, TaskState, ValidatedTaskChunk},
    task_subgraph_store::TaskSubgraphStore,
};
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use baml_rt_provenance::{ProvenanceWriter, SurrealStoreBuilder, TaskGraphReader};

async fn build_store() -> Arc<TaskSubgraphStore> {
    let prov = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("isolated provenance store");
    let reader: Arc<dyn TaskGraphReader> = prov.clone();
    let writer: Arc<dyn ProvenanceWriter> = prov.clone();
    Arc::new(TaskSubgraphStore::new(reader, writer))
}

#[tokio::test]
async fn apply_task_delta_concurrent_same_task_valid_final_state() {
    let store = build_store().await;
    let task_id = TaskId::from_external(ExternalId::new("concurrent-task-1"));
    let context_id = ContextId::new(1, 1);

    let chunk1_task = common::minimal_task(
        &task_id,
        &context_id,
        Some(common::task_status("TASK_STATE_SUBMITTED")),
    );
    store
        .apply_task_chunk(
            ValidatedTaskChunk::try_from(StreamResponse {
                task: Some(chunk1_task),
                ..Default::default()
            })
            .unwrap(),
        )
        .await
        .unwrap();

    let chunk2_task = common::minimal_task(&task_id, &context_id, None);
    store
        .apply_task_chunk(
            ValidatedTaskChunk::try_from(StreamResponse {
                task: Some(chunk2_task),
                status_update: Some(baml_rt_a2a::a2a_types::TaskStatusUpdateEvent {
                    context_id: Some(context_id.clone()),
                    task_id: Some(task_id.clone()),
                    status: Some(common::task_status("TASK_STATE_WORKING")),
                    metadata: None,
                    extra: HashMap::new(),
                }),
                ..Default::default()
            })
            .unwrap(),
        )
        .await
        .unwrap();

    let store_a = store.clone();
    let task_id_a = task_id.clone();
    let context_id_a = context_id.clone();
    let chunk_a = common::minimal_task(&task_id_a, &context_id_a, None);
    let handle_a = tokio::spawn(async move {
        store_a
            .apply_task_chunk(
                ValidatedTaskChunk::try_from(StreamResponse {
                    task: Some(chunk_a),
                    status_update: Some(baml_rt_a2a::a2a_types::TaskStatusUpdateEvent {
                        context_id: Some(context_id_a.clone()),
                        task_id: Some(task_id_a.clone()),
                        status: Some(common::task_status("TASK_STATE_AUTH_REQUIRED")),
                        metadata: None,
                        extra: HashMap::new(),
                    }),
                    ..Default::default()
                })
                .unwrap(),
            )
            .await
    });

    let store_b = store.clone();
    let task_id_b = task_id.clone();
    let context_id_b = context_id.clone();
    let chunk_b = common::minimal_task(&task_id_b, &context_id_b, None);
    let handle_b = tokio::spawn(async move {
        store_b
            .apply_task_chunk(
                ValidatedTaskChunk::try_from(StreamResponse {
                    task: Some(chunk_b),
                    status_update: Some(baml_rt_a2a::a2a_types::TaskStatusUpdateEvent {
                        context_id: Some(context_id_b.clone()),
                        task_id: Some(task_id_b.clone()),
                        status: Some(common::task_status("TASK_STATE_COMPLETED")),
                        metadata: None,
                        extra: HashMap::new(),
                    }),
                    ..Default::default()
                })
                .unwrap(),
            )
            .await
    });

    let _ = handle_a.await.unwrap();
    let _ = handle_b.await.unwrap();

    let task = store
        .get(task_id.as_str(), None)
        .await
        .expect("task exists");
    let state = task
        .status
        .as_ref()
        .and_then(|status| status.state.as_ref())
        .and_then(|state| match state {
            TaskState::String(value) => Some(value.as_str()),
            TaskState::Integer(_) => None,
        });
    assert!(state.is_some(), "final state must be present");
    assert!(
        matches!(
            state,
            Some("TASK_STATE_AUTH_REQUIRED" | "TASK_STATE_COMPLETED")
        ),
        "final state must be one of the concurrent writes, got {state:?}",
    );
}
