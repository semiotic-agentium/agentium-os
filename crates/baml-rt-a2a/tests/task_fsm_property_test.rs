// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![recursion_limit = "256"]

//! Graph-backed task lifecycle invariants.

mod common;

use std::{collections::HashMap, sync::Arc};

use baml_rt_a2a::{
    a2a_store::{TaskEventRecorder, TaskRepository},
    a2a_types::{Message, Part, TaskState, TaskStatus},
    task_subgraph_store::TaskSubgraphStore,
};
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use baml_rt_provenance::{ProvenanceWriter, SurrealStoreBuilder, TaskGraphReader};

const S_SUBMITTED: &str = "TASK_STATE_SUBMITTED";
const S_WORKING: &str = "TASK_STATE_WORKING";
const S_COMPLETED: &str = "TASK_STATE_COMPLETED";
const S_FAILED: &str = "TASK_STATE_FAILED";
const S_CANCELED: &str = "TASK_STATE_CANCELED";
const S_REJECTED: &str = "TASK_STATE_REJECTED";
const S_INPUT_REQUIRED: &str = "TASK_STATE_INPUT_REQUIRED";
const S_AUTH_REQUIRED: &str = "TASK_STATE_AUTH_REQUIRED";

async fn build_store() -> Arc<TaskSubgraphStore> {
    let prov = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("isolated provenance store");
    let reader: Arc<dyn TaskGraphReader> = prov.clone();
    let writer: Arc<dyn ProvenanceWriter> = prov.clone();
    Arc::new(TaskSubgraphStore::new(reader, writer))
}

async fn seed_task_and_submitted(
    store: &TaskSubgraphStore,
    task_id: &TaskId,
    context_id: &ContextId,
) {
    store
        .ensure_task_exists(task_id, Some(context_id))
        .await
        .expect("task exists");
    let result = store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            common::task_status(S_SUBMITTED),
        )
        .await
        .expect("submitted write");
    assert!(result.is_some(), "submitted seed must succeed");
}

fn input_required_status(prompt: &str) -> TaskStatus {
    TaskStatus {
        state: Some(TaskState::String(S_INPUT_REQUIRED.to_string())),
        message: Some(Message {
            message_id: baml_rt_a2a::a2a_types::A2aMessageId::incoming(ExternalId::new(
                "task-fsm-input-required",
            )),
            role: baml_rt_a2a::a2a_types::MessageRole::Agent,
            parts: vec![Part {
                text: Some(prompt.to_string()),
                ..Default::default()
            }],
            context_id: None,
            task_id: None,
            reference_task_ids: Vec::new(),
            extensions: Vec::new(),
            metadata: None,
            extra: HashMap::new(),
        }),
        timestamp: None,
        extra: HashMap::new(),
    }
}

fn failed_status(reason: &str) -> TaskStatus {
    let mut extra = HashMap::new();
    extra.insert(
        "error_reason".to_string(),
        serde_json::Value::String(reason.to_string()),
    );
    TaskStatus {
        state: Some(TaskState::String(S_FAILED.to_string())),
        message: None,
        timestamp: None,
        extra,
    }
}

fn task_state(task: &baml_rt_a2a::a2a_types::Task) -> Option<&str> {
    task.status
        .as_ref()
        .and_then(|status| status.state.as_ref())
        .and_then(|state| match state {
            baml_rt_a2a::a2a_types::TaskState::String(value) => Some(value.as_str()),
            baml_rt_a2a::a2a_types::TaskState::Integer(_) => None,
        })
}

#[tokio::test]
async fn graph_store_accepts_valid_transitions_and_rejects_invalid_ones() {
    let store = build_store().await;
    let task_id = TaskId::from_external(ExternalId::new("task-fsm-valid"));
    let context_id = ContextId::new(1, 1);

    seed_task_and_submitted(&store, &task_id, &context_id).await;

    let working = store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            common::task_status(S_WORKING),
        )
        .await
        .expect("working write");
    assert!(working.is_some(), "submitted -> working must succeed");

    let invalid = store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            common::task_status(S_SUBMITTED),
        )
        .await
        .expect("invalid write returns none");
    assert!(invalid.is_none(), "working -> submitted must be rejected");

    let input_required = store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            input_required_status("need input"),
        )
        .await
        .expect("input required write");
    assert!(
        input_required.is_some(),
        "working -> input_required must succeed"
    );
}

#[tokio::test]
async fn graph_store_rejects_non_submitted_first_status() {
    let store = build_store().await;
    let task_id = TaskId::from_external(ExternalId::new("task-fsm-first"));
    let context_id = ContextId::new(1, 2);

    store
        .ensure_task_exists(&task_id, Some(&context_id))
        .await
        .expect("task exists");

    for status in [
        common::task_status(S_WORKING),
        common::task_status(S_COMPLETED),
        failed_status("boom"),
        common::task_status(S_CANCELED),
        common::task_status(S_REJECTED),
        input_required_status("need input"),
        common::task_status(S_AUTH_REQUIRED),
    ] {
        let result = store
            .record_status_update(task_id.clone(), context_id.clone(), status.clone())
            .await
            .expect("first status write");
        assert!(
            result.is_none(),
            "first status {:?} must be rejected",
            status.state,
        );
    }
}

#[tokio::test]
async fn ensure_task_exists_preserves_existing_status() {
    let store = build_store().await;
    let task_id = TaskId::from_external(ExternalId::new("task-fsm-preserve"));
    let context_id = ContextId::new(1, 3);

    seed_task_and_submitted(&store, &task_id, &context_id).await;
    store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            common::task_status(S_WORKING),
        )
        .await
        .expect("working write");

    store
        .ensure_task_exists(&task_id, Some(&context_id))
        .await
        .expect("ensure existing task");

    let task = store
        .get(task_id.as_str(), None)
        .await
        .expect("task must still exist");
    assert_eq!(
        task_state(&task),
        Some(S_WORKING),
        "ensure_task_exists must not overwrite the current state",
    );
}

#[tokio::test]
async fn terminal_state_rejects_further_updates() {
    let store = build_store().await;
    let task_id = TaskId::from_external(ExternalId::new("task-fsm-terminal"));
    let context_id = ContextId::new(1, 4);

    seed_task_and_submitted(&store, &task_id, &context_id).await;
    let completed = store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            common::task_status(S_COMPLETED),
        )
        .await
        .expect("completed write");
    assert!(completed.is_some(), "submitted -> completed must succeed");

    let rejected = store
        .record_status_update(
            task_id.clone(),
            context_id.clone(),
            common::task_status(S_WORKING),
        )
        .await
        .expect("post-terminal write");
    assert!(
        rejected.is_none(),
        "terminal tasks must reject further updates"
    );
}
