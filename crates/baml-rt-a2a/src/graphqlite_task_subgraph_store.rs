//! A2A adapter over provenance-owned graph persistence.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_provenance::{
    A2aGraphStore, GraphqliteProvenanceStore, ProvenanceContextReader,
    ProvenanceConversationContextItem, TaskSubgraphNode,
};

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
}

impl GraphqliteTaskSubgraphStore {
    pub fn new(store: Arc<GraphqliteProvenanceStore>) -> Self {
        let context_reader = store.clone() as Arc<dyn ProvenanceContextReader>;
        let graph = store as Arc<dyn A2aGraphStore>;
        Self {
            graph,
            context_reader,
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
        let _ = self
            .record_status_update(Some(task_id), context_id, status)
            .await;
        self.get(id, None).await
    }

    async fn insert_message(&self, message: &Message) -> Result<()> {
        let Some(task_id) = message.task_id.as_ref().map(|t| t.as_str().to_string()) else {
            return Ok(());
        };
        let seq = self
            .graph
            .max_seq_for_label("A2ATaskMessageSubgraph", &task_id)
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
        let status_json = serde_json::to_string(&status)
            .map_err(|e| BamlRtError::InvalidArgument(format!("serialize status: {e}")))?;
        self.graph
            .set_task_status_json(task_id.as_str(), &status_json)
            .await
            .map_err(map_store_err)?;
        let seq = self
            .graph
            .max_seq_for_label("A2ATaskUpdateSubgraph", task_id.as_str())
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
        let seq = self
            .graph
            .max_seq_for_label("A2ATaskUpdateSubgraph", task_id.as_str())
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
            let _ = self.graph.delete_update_node(&id).await;
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
