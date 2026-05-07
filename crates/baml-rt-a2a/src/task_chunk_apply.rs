//! Shared I2 chunk application logic for [`crate::a2a_store::TaskStore`].

use baml_rt_core::ids::{ContextId, TaskId};

use crate::{
    a2a_store::{TaskStore, TaskUpdateEvent, status_to_string},
    a2a_types::{
        Message, Task, TaskArtifactUpdateEvent, TaskStatusUpdateEvent, ValidatedTaskChunk,
    },
};

pub(crate) fn apply_validated_chunk_to_task_store(
    store: &mut TaskStore,
    chunk: &ValidatedTaskChunk,
) -> Vec<TaskUpdateEvent> {
    apply_chunk_fields(
        store,
        chunk.task().cloned(),
        chunk.message().cloned(),
        chunk.status_update(),
        chunk.artifact_update(),
    )
}

fn apply_chunk_fields(
    store: &mut TaskStore,
    task: Option<Task>,
    message: Option<Message>,
    status_update: Option<&TaskStatusUpdateEvent>,
    artifact_update: Option<&TaskArtifactUpdateEvent>,
) -> Vec<TaskUpdateEvent> {
    let mut out = Vec::new();
    let mut status_recorded_from_task: Option<(Option<TaskId>, Option<ContextId>, String)> = None;
    if let Some(mut t) = task {
        let status = t.status.take();
        let context_id = t.context_id.clone();
        let task_id = t.id.clone();
        let artifacts = std::mem::take(&mut t.artifacts);
        let result = store.upsert(t);
        debug_assert!(
            result.is_some(),
            "apply_task_chunk: task without id is a logic error"
        );
        if let Some(status) = status
            && let Some(ev) =
                store.record_status_update(task_id.clone(), context_id.clone(), status.clone())
        {
            let state_str = status_to_string(&status).unwrap_or_default();
            status_recorded_from_task = Some((task_id.clone(), context_id.clone(), state_str));
            out.push(ev);
        }
        if let Some(tid) = task_id {
            for artifact in artifacts {
                if let Some(ev) = store.record_artifact_update(
                    Some(tid.clone()),
                    context_id.clone(),
                    artifact,
                    Some(false),
                    Some(true),
                ) {
                    out.push(ev);
                }
            }
        }
    }
    if let Some(msg) = message {
        store.insert_message(&msg);
    }
    if let Some(up) = status_update
        && let Some(status) = up.status.clone()
    {
        let state_str = status_to_string(&status).unwrap_or_default();
        let is_duplicate = status_recorded_from_task
            .as_ref()
            .is_some_and(|(tid, cid, s)| {
                tid == &up.task_id && cid == &up.context_id && *s == state_str
            });
        if !is_duplicate
            && let Some(ev) =
                store.record_status_update(up.task_id.clone(), up.context_id.clone(), status)
        {
            out.push(ev);
        }
    }
    if let Some(up) = artifact_update
        && let Some(ev) = store.record_artifact_update(
            up.task_id.clone(),
            up.context_id.clone(),
            up.artifact.clone().unwrap_or_default(),
            up.append,
            up.last_chunk,
        )
    {
        out.push(ev);
    }
    out
}
