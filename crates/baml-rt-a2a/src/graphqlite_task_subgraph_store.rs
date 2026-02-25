//! A2A adapter over provenance-owned graph persistence.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_provenance::{
    A2aGraphStore, ProvenanceContextReader, ProvenanceConversationContextItem, TaskSubgraphNode,
};
use tokio::sync::Mutex as TokioMutex;
use tracing::warn;

use crate::{
    a2a_store::{
        ConversationContextSource, TaskChunkApplier, TaskEventRecorder, TaskRepository,
        TaskUpdateEvent, TaskUpdateQueue,
    },
    a2a_types::{
        Artifact, ListTasksRequest, ListTasksResponse, Message, Task, TaskArtifactUpdateEvent,
        TaskState, TaskStatus, TaskStatusUpdateEvent,
    },
};

const S_SUBMITTED: &str = "TASK_STATE_SUBMITTED";
const S_WORKING: &str = "TASK_STATE_WORKING";
const S_COMPLETED: &str = "TASK_STATE_COMPLETED";
const S_FAILED: &str = "TASK_STATE_FAILED";
const S_CANCELED: &str = "TASK_STATE_CANCELED";
const S_REJECTED: &str = "TASK_STATE_REJECTED";
const S_INPUT_REQUIRED: &str = "TASK_STATE_INPUT_REQUIRED";
const S_AUTH_REQUIRED: &str = "TASK_STATE_AUTH_REQUIRED";

fn is_terminal_state(s: &str) -> bool {
    matches!(s, S_COMPLETED | S_FAILED | S_CANCELED | S_REJECTED)
}

fn is_allowed_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if is_terminal_state(from) {
        return false;
    }
    matches!(
        (from, to),
        (S_SUBMITTED, S_WORKING)
            | (S_SUBMITTED, S_COMPLETED)
            | (S_SUBMITTED, S_FAILED)
            | (S_SUBMITTED, S_CANCELED)
            | (S_SUBMITTED, S_REJECTED)
            | (S_SUBMITTED, S_INPUT_REQUIRED)
            | (S_SUBMITTED, S_AUTH_REQUIRED)
            | (S_WORKING, S_INPUT_REQUIRED)
            | (S_WORKING, S_AUTH_REQUIRED)
            | (S_WORKING, S_COMPLETED)
            | (S_WORKING, S_FAILED)
            | (S_WORKING, S_CANCELED)
            | (S_WORKING, S_REJECTED)
            | (S_INPUT_REQUIRED, S_WORKING)
            | (S_INPUT_REQUIRED, S_CANCELED)
            | (S_INPUT_REQUIRED, S_REJECTED)
            | (S_INPUT_REQUIRED, S_COMPLETED)
            | (S_INPUT_REQUIRED, S_FAILED)
            | (S_AUTH_REQUIRED, S_WORKING)
            | (S_AUTH_REQUIRED, S_CANCELED)
            | (S_AUTH_REQUIRED, S_REJECTED)
            | (S_AUTH_REQUIRED, S_COMPLETED)
            | (S_AUTH_REQUIRED, S_FAILED)
    )
}

fn status_to_string(status: &TaskStatus) -> Option<String> {
    status.state.as_ref().map(|state| match state {
        TaskState::String(value) => value.clone(),
        TaskState::Integer(value) => value.to_string(),
    })
}

fn map_store_err(e: String) -> BamlRtError {
    BamlRtError::ProvenanceContextRead {
        source: Box::new(std::io::Error::other(e)),
    }
}

pub struct GraphqliteTaskSubgraphStore {
    graph: Arc<dyn A2aGraphStore>,
    context_reader: Arc<dyn ProvenanceContextReader>,
    mutation_lock: TokioMutex<()>,
}

impl GraphqliteTaskSubgraphStore {
    pub fn new(
        graph: Arc<dyn A2aGraphStore>,
        context_reader: Arc<dyn ProvenanceContextReader>,
    ) -> Self {
        Self {
            graph,
            context_reader,
            mutation_lock: TokioMutex::new(()),
        }
    }
}

#[async_trait]
impl TaskRepository for GraphqliteTaskSubgraphStore {
    async fn upsert(&self, task: Task) -> Result<Option<Task>> {
        let id = task
            .id
            .as_ref()
            .ok_or_else(|| BamlRtError::InvalidArgument("task.id required".into()))?
            .as_str()
            .to_string();
        let context_id = task
            .context_id
            .as_ref()
            .map(|c| c.as_str().to_string())
            .unwrap_or_default();
        let _guard = self.mutation_lock.lock().await;
        let preserve_status = self
            .graph
            .get_task_node(&id)
            .await
            .map_err(map_store_err)?
            .and_then(|n| (!n.status_json.is_empty()).then_some(n.status_json))
            .and_then(|raw| serde_json::from_str::<TaskStatus>(&raw).ok());
        let status_to_store = preserve_status.clone().or(task.status.clone());
        let node = TaskSubgraphNode {
            id: id.clone(),
            context_id,
            status_json: status_to_store
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| BamlRtError::InvalidArgument(format!("serialize status: {e}")))?
                .unwrap_or_default(),
            metadata_json: task
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| BamlRtError::InvalidArgument(format!("serialize metadata: {e}")))?
                .unwrap_or_default(),
            extra_json: serde_json::to_string(&task.extra)
                .map_err(|e| BamlRtError::InvalidArgument(format!("serialize extra: {e}")))?,
            artifacts_json: serde_json::to_string(&task.artifacts)
                .map_err(|e| BamlRtError::InvalidArgument(format!("serialize artifacts: {e}")))?,
        };
        let ord = self.graph.max_task_ord().await.map_err(map_store_err)? + 1;
        self.graph
            .upsert_task_node(&node, ord)
            .await
            .map_err(map_store_err)?;
        let mut out = task;
        out.status = status_to_store;
        Ok(Some(out))
    }

    async fn ensure_task_exists(
        &self,
        task_id: &TaskId,
        context_id: Option<&ContextId>,
    ) -> Result<()> {
        let _guard = self.mutation_lock.lock().await;
        let ord = self.graph.max_task_ord().await.map_err(map_store_err)?;
        self.graph
            .ensure_task_node(
                task_id.as_str(),
                context_id.map(|c| c.as_str()).unwrap_or_default(),
                ord + 1,
            )
            .await
            .map_err(map_store_err)
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let node = self.graph.get_task_node(id).await.ok().flatten()?;
        let mut history: Vec<Message> = self
            .graph
            .list_message_json(id)
            .await
            .ok()?
            .into_iter()
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        if let Some(limit) = history_length {
            if limit == 0 {
                history.clear();
            } else if history.len() > limit {
                history = history.split_off(history.len() - limit);
            }
        }
        let status = if node.status_json.is_empty() {
            None
        } else {
            serde_json::from_str(&node.status_json).ok()
        };
        let metadata = if node.metadata_json.is_empty() {
            None
        } else {
            serde_json::from_str(&node.metadata_json).ok()
        };
        let extra = serde_json::from_str(&node.extra_json).ok()?;
        let artifacts = serde_json::from_str(&node.artifacts_json).ok()?;
        Some(Task {
            id: Some(TaskId::from_external(ExternalId::new(node.id))),
            context_id: ContextId::parse_temporal(&node.context_id),
            artifacts,
            history,
            status,
            metadata,
            extra,
        })
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let rows = match self
            .graph
            .list_task_nodes(request.context_id.as_ref().map(|c| c.as_str()))
            .await
        {
            Ok(r) => r,
            Err(_) => {
                return ListTasksResponse {
                    tasks: vec![],
                    next_page_token: None,
                    total_size: Some(0),
                    page_size: None,
                    extra: HashMap::new(),
                };
            }
        };
        let history_limit = request.history_length.as_ref().and_then(|v| v.as_usize());
        let mut tasks = Vec::new();
        for node in rows {
            if let Some(task) = self.get(&node.id, history_limit).await {
                tasks.push(task);
            }
        }

        if let Some(status) = &request.status {
            tasks.retain(|task| {
                task.status
                    .as_ref()
                    .and_then(|s| s.state.as_ref())
                    .map(|s| match (s, status) {
                        (TaskState::String(a), TaskState::String(b)) => a == b,
                        (TaskState::Integer(a), TaskState::Integer(b)) => a == b,
                        _ => false,
                    })
                    .unwrap_or(false)
            });
        }

        if !request.include_artifacts.unwrap_or(false) {
            for task in &mut tasks {
                task.artifacts.clear();
            }
        }

        let total_size = tasks.len() as u64;
        let page_size = request
            .page_size
            .as_ref()
            .and_then(|v| v.as_usize())
            .unwrap_or(50);
        let start = request
            .page_token
            .as_ref()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let end = std::cmp::min(start + page_size, tasks.len());
        let page_tasks = if start < tasks.len() {
            tasks[start..end].to_vec()
        } else {
            vec![]
        };
        let next_page_token = if end < tasks.len() {
            Some(end.to_string())
        } else {
            None
        };

        ListTasksResponse {
            tasks: page_tasks,
            next_page_token,
            total_size: Some(total_size),
            page_size: Some(page_size as u64),
            extra: HashMap::new(),
        }
    }

    async fn cancel(&self, id: &str) -> Option<Task> {
        let task = self.get(id, None).await?;
        let task_id = task.id.clone()?;
        let context_id = task.context_id.clone();
        let status = TaskStatus {
            state: Some(TaskState::String(S_CANCELED.to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };
        if let Err(err) = self
            .record_status_update(Some(task_id), context_id, status)
            .await
        {
            warn!(task_id = %id, error = %err, "failed to persist cancel status update");
        }
        self.get(id, None).await
    }

    async fn insert_message(&self, message: &Message) -> Result<()> {
        let Some(task_id) = message.task_id.as_ref().map(|t| t.as_str().to_string()) else {
            return Ok(());
        };
        let _guard = self.mutation_lock.lock().await;
        let seq = self
            .graph
            .max_message_seq(&task_id)
            .await
            .map_err(map_store_err)?
            + 1;
        let message_json = serde_json::to_string(message)
            .map_err(|e| BamlRtError::InvalidArgument(format!("serialize message: {e}")))?;
        let node_id = format!("{task_id}:msg:{seq}");
        self.graph
            .insert_message_node(&node_id, &task_id, seq, &message_json)
            .await
            .map_err(map_store_err)
    }
}

#[async_trait]
impl TaskEventRecorder for GraphqliteTaskSubgraphStore {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let task_id = match task_id {
            Some(t) => t,
            None => return Ok(None),
        };
        let _guard = self.mutation_lock.lock().await;
        let new_state = match status_to_string(&status) {
            Some(s) => s,
            None => return Ok(None),
        };
        let current_state = self
            .graph
            .get_task_node(task_id.as_str())
            .await
            .ok()
            .flatten()
            .and_then(|n| (!n.status_json.is_empty()).then_some(n.status_json))
            .and_then(|raw| serde_json::from_str::<TaskStatus>(&raw).ok())
            .and_then(|current_status| status_to_string(&current_status));
        let allowed = match current_state.as_deref() {
            None => new_state == S_SUBMITTED,
            Some(current) if is_terminal_state(current) => false,
            Some(current) => is_allowed_transition(current, &new_state),
        };
        if !allowed {
            return Ok(None);
        }
        self.graph
            .ensure_task_node(
                task_id.as_str(),
                context_id.as_ref().map(|c| c.as_str()).unwrap_or_default(),
                self.graph.max_task_ord().await.map_err(map_store_err)? + 1,
            )
            .await
            .map_err(map_store_err)?;
        let status_json = serde_json::to_string(&status)
            .map_err(|e| BamlRtError::InvalidArgument(format!("serialize status: {e}")))?;
        self.graph
            .set_task_status_json(task_id.as_str(), &status_json)
            .await
            .map_err(map_store_err)?;
        let seq = self
            .graph
            .max_update_seq(task_id.as_str())
            .await
            .map_err(map_store_err)?
            + 1;
        let payload_json = serde_json::to_string(&serde_json::json!({
            "context_id": context_id.as_ref().map(|c| c.as_str()),
            "task_id": task_id.as_str(),
            "status": status
        }))
        .map_err(|e| BamlRtError::InvalidArgument(format!("serialize payload: {e}")))?;
        let node_id = format!("{}:update:{seq}", task_id.as_str());
        self.graph
            .insert_update_node(&node_id, task_id.as_str(), seq, "status", &payload_json)
            .await
            .map_err(map_store_err)?;
        Ok(Some(TaskUpdateEvent::Status(TaskStatusUpdateEvent {
            context_id,
            task_id: Some(task_id),
            status: Some(status),
            metadata: None,
            extra: HashMap::new(),
        })))
    }

    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let task_id = match task_id {
            Some(t) => t,
            None => return Ok(None),
        };
        let _guard = self.mutation_lock.lock().await;
        self.graph
            .ensure_task_node(
                task_id.as_str(),
                context_id.as_ref().map(|c| c.as_str()).unwrap_or_default(),
                self.graph.max_task_ord().await.map_err(map_store_err)? + 1,
            )
            .await
            .map_err(map_store_err)?;
        let seq = self
            .graph
            .max_update_seq(task_id.as_str())
            .await
            .map_err(map_store_err)?
            + 1;
        let payload_json = serde_json::to_string(&serde_json::json!({
            "context_id": context_id.as_ref().map(|c| c.as_str()),
            "task_id": task_id.as_str(),
            "last_chunk": last_chunk,
            "append": append,
            "artifact": artifact
        }))
        .map_err(|e| BamlRtError::InvalidArgument(format!("serialize payload: {e}")))?;
        let node_id = format!("{}:update:{seq}", task_id.as_str());
        self.graph
            .insert_update_node(&node_id, task_id.as_str(), seq, "artifact", &payload_json)
            .await
            .map_err(map_store_err)?;
        Ok(Some(TaskUpdateEvent::Artifact(TaskArtifactUpdateEvent {
            context_id,
            task_id: Some(task_id),
            last_chunk,
            append,
            artifact: Some(artifact),
            metadata: None,
            extra: HashMap::new(),
        })))
    }
}

#[async_trait]
impl TaskUpdateQueue for GraphqliteTaskSubgraphStore {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        let rows = match self.graph.list_update_nodes(task_id).await {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut ids = Vec::new();
        let events: Vec<TaskUpdateEvent> = rows
            .iter()
            .filter_map(|row| {
                ids.push(row.id.clone());
                match row.kind.as_str() {
                    "status" => serde_json::from_str::<TaskStatusUpdateEvent>(&row.payload_json)
                        .ok()
                        .map(TaskUpdateEvent::Status),
                    "artifact" => {
                        serde_json::from_str::<TaskArtifactUpdateEvent>(&row.payload_json)
                            .ok()
                            .map(TaskUpdateEvent::Artifact)
                    }
                    _ => None,
                }
            })
            .collect();
        for id in ids {
            if let Err(err) = self.graph.delete_update_node(&id).await {
                warn!(update_id = %id, error = %err, "failed to delete drained task update node");
            }
        }
        events
    }
}

#[async_trait]
impl TaskChunkApplier for GraphqliteTaskSubgraphStore {
    async fn apply_task_delta(
        &self,
        task: Option<Task>,
        message: Option<Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Result<Vec<TaskUpdateEvent>> {
        if task.is_none() && (status_update.is_some() || artifact_update.is_some()) {
            return Err(BamlRtError::InvalidArgument(
                "status_update or artifact_update requires task in chunk".into(),
            ));
        }
        let mut out = Vec::new();
        if let Some(mut t) = task {
            let status = t.status.take();
            let context_id = t.context_id.clone();
            let task_id = t.id.clone();
            let artifacts = std::mem::take(&mut t.artifacts);
            let _ = self.upsert(t).await?;
            if let Some(status) = status
                && let Some(tid) = &task_id
                && let Some(ev) = self
                    .record_status_update(Some(tid.clone()), context_id.clone(), status)
                    .await?
            {
                out.push(ev);
            }
            if let Some(tid) = task_id {
                for artifact in artifacts {
                    if let Some(ev) = self
                        .record_artifact_update(
                            Some(tid.clone()),
                            None,
                            artifact,
                            Some(false),
                            Some(true),
                        )
                        .await?
                    {
                        out.push(ev);
                    }
                }
            }
        }
        if let Some(msg) = message {
            self.insert_message(&msg).await?;
        }
        if let Some(ref up) = status_update
            && let Some(status) = up.status.clone()
            && let Some(ev) = self
                .record_status_update(up.task_id.clone(), up.context_id.clone(), status)
                .await?
        {
            out.push(ev);
        }
        if let Some(ref up) = artifact_update
            && let Some(ev) = self
                .record_artifact_update(
                    up.task_id.clone(),
                    up.context_id.clone(),
                    up.artifact.clone().unwrap_or_default(),
                    up.append,
                    up.last_chunk,
                )
                .await?
        {
            out.push(ev);
        }
        Ok(out)
    }
}

#[async_trait]
impl ConversationContextSource for GraphqliteTaskSubgraphStore {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.context_reader
            .conversation_context(context_id, limit)
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
    use baml_rt_provenance::GraphqliteStoreBuilder;

    use super::*;

    fn test_store() -> (
        GraphqliteTaskSubgraphStore,
        Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
    ) {
        let prov = GraphqliteStoreBuilder::in_memory()
            .build()
            .expect("build graphqlite store");
        let graph: Arc<dyn A2aGraphStore> = prov.clone();
        let context_reader: Arc<dyn ProvenanceContextReader> = prov.clone();
        (
            GraphqliteTaskSubgraphStore::new(graph, context_reader),
            prov,
        )
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips_task_node() {
        let (store, _prov) = test_store();
        let task = Task {
            id: Some(TaskId::from_external(ExternalId::new(
                "task-upsert-roundtrip",
            ))),
            context_id: Some(ContextId::new(1, 1)),
            artifacts: Vec::new(),
            history: Vec::new(),
            status: None,
            metadata: None,
            extra: HashMap::new(),
        };
        store.upsert(task).await.expect("upsert task");

        let raw = store
            .graph
            .get_task_node("task-upsert-roundtrip")
            .await
            .expect("raw graph get");
        assert!(raw.is_some(), "expected raw task node in graph");

        let got = store.get("task-upsert-roundtrip", None).await;
        assert!(
            got.is_some(),
            "expected task node to be visible immediately"
        );
    }

    #[tokio::test]
    async fn apply_task_delta_with_task_status_creates_readable_task() {
        let (store, _prov) = test_store();
        let task = Task {
            id: Some(TaskId::from_external(ExternalId::new("task-apply-delta"))),
            context_id: Some(ContextId::new(2, 1)),
            artifacts: Vec::new(),
            history: Vec::new(),
            status: Some(TaskStatus {
                state: Some(TaskState::String(S_SUBMITTED.to_string())),
                message: None,
                timestamp: None,
                extra: HashMap::new(),
            }),
            metadata: None,
            extra: HashMap::new(),
        };
        let events = store
            .apply_task_delta(Some(task), None, None, None)
            .await
            .expect("apply delta");
        assert!(
            !events.is_empty(),
            "expected status event from apply_task_delta with submitted status"
        );

        let got = store.get("task-apply-delta", None).await;
        assert!(got.is_some(), "expected task after apply_task_delta");
    }

    #[tokio::test]
    async fn record_status_update_creates_task_with_context_id() {
        let (store, _prov) = test_store();
        let task_id = TaskId::from_external(ExternalId::new("task-status-create"));
        let context_id = ContextId::new(3, 1);
        let status = TaskStatus {
            state: Some(TaskState::String(S_SUBMITTED.to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };

        let out = store
            .record_status_update(Some(task_id.clone()), Some(context_id.clone()), status)
            .await
            .expect("record status");
        assert!(out.is_some(), "expected accepted status update");

        let task = store
            .get(task_id.as_str(), None)
            .await
            .expect("task should exist after status update");
        assert_eq!(
            task.context_id.as_ref().map(|id| id.as_str()),
            Some(context_id.as_str()),
            "ensure_task_node should preserve context_id for subscribe/get paths"
        );
    }

    #[tokio::test]
    async fn record_status_update_rejected_initial_state_does_not_create_placeholder_task() {
        let (store, _prov) = test_store();
        let task_id = TaskId::from_external(ExternalId::new("task-status-reject"));
        let context_id = ContextId::new(4, 1);
        let invalid_first_status = TaskStatus {
            state: Some(TaskState::String(S_WORKING.to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };

        let out = store
            .record_status_update(
                Some(task_id.clone()),
                Some(context_id.clone()),
                invalid_first_status,
            )
            .await
            .expect("record status");
        assert!(
            out.is_none(),
            "invalid initial transition should be rejected"
        );
        assert!(
            store.get(task_id.as_str(), None).await.is_none(),
            "rejected status update must not leave a placeholder task row"
        );
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips_task_id_with_control_whitespace() {
        let (store, _prov) = test_store();
        let weird_id = "task-line\tbreak\nsegment\rend";
        let task = Task {
            id: Some(TaskId::from_external(ExternalId::new(weird_id))),
            context_id: Some(ContextId::new(5, 1)),
            artifacts: Vec::new(),
            history: Vec::new(),
            status: None,
            metadata: None,
            extra: HashMap::new(),
        };
        store.upsert(task).await.expect("upsert task");
        store
            .graph
            .insert_message_node(
                "weird-msg-1",
                weird_id,
                1,
                r#"{"role":"user","parts":[{"text":"hello"}]}"#,
            )
            .await
            .expect("insert message");
        let submitted = TaskStatus {
            state: Some(TaskState::String(S_SUBMITTED.to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };
        let status_event = store
            .record_status_update(
                Some(TaskId::from_external(ExternalId::new(weird_id))),
                Some(ContextId::new(5, 1)),
                submitted,
            )
            .await
            .expect("record status for weird id");
        assert!(
            status_event.is_some(),
            "status update should be accepted for weird-id task"
        );

        let got = store.get(weird_id, None).await;
        assert!(
            got.is_some(),
            "expected control-whitespace task id to round-trip"
        );
        let message_rows = store
            .graph
            .list_message_json(weird_id)
            .await
            .expect("list messages");
        assert_eq!(
            message_rows.len(),
            1,
            "message lookup should match weird task_id"
        );
        assert_eq!(
            store
                .graph
                .max_message_seq(weird_id)
                .await
                .expect("max message seq"),
            1
        );
        assert_eq!(
            store
                .graph
                .max_update_seq(weird_id)
                .await
                .expect("max update seq"),
            1
        );
        let updates = store
            .graph
            .list_update_nodes(weird_id)
            .await
            .expect("list updates");
        assert_eq!(updates.len(), 1, "update lookup should match weird task_id");
        let raw = store
            .graph
            .get_task_node(weird_id)
            .await
            .expect("raw graph get");
        assert!(
            raw.is_some(),
            "raw graph lookup should handle escaped id literal"
        );
    }
}
