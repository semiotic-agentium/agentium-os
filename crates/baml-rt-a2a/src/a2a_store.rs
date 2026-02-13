use crate::a2a_types::{
    Artifact, ListTasksRequest, ListTasksResponse, Message, MessageRole, ROLE_USER,
    TASK_STATE_CANCELED, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};
use async_trait::async_trait;
use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use baml_rt_observability::metrics;
use baml_rt_provenance::{ProvEvent, ProvenanceWriter};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub enum TaskUpdateEvent {
    Status(TaskStatusUpdateEvent),
    Artifact(TaskArtifactUpdateEvent),
}

impl TaskUpdateEvent {
    pub fn task_id(&self) -> Option<&str> {
        match self {
            TaskUpdateEvent::Status(event) => event.task_id.as_ref().map(|id| id.as_str()),
            TaskUpdateEvent::Artifact(event) => event.task_id.as_ref().map(|id| id.as_str()),
        }
    }
}

#[derive(Debug, Default)]
pub struct TaskStore {
    tasks: HashMap<String, Task>,
    order: Vec<String>,
    updates: HashMap<String, Vec<TaskUpdateEvent>>,
}

#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn upsert(&self, task: Task) -> Option<Task>;
    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task>;
    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse;
    async fn cancel(&self, id: &str) -> Option<Task>;
    async fn insert_message(&self, message: &Message);
}

#[async_trait]
pub trait TaskEventRecorder: Send + Sync {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Option<TaskUpdateEvent>;
    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Option<TaskUpdateEvent>;
}

#[async_trait]
pub trait TaskUpdateQueue: Send + Sync {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent>;
}

#[async_trait]
pub trait TaskStoreBackend: TaskRepository + TaskEventRecorder + TaskUpdateQueue {}

impl<T> TaskStoreBackend for T where T: TaskRepository + TaskEventRecorder + TaskUpdateQueue {}

#[async_trait]
impl TaskRepository for Mutex<TaskStore> {
    async fn upsert(&self, task: Task) -> Option<Task> {
        let mut store = self.lock().await;
        store.upsert(task)
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let store = self.lock().await;
        store.get(id, history_length)
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let store = self.lock().await;
        store.list(request)
    }

    async fn cancel(&self, id: &str) -> Option<Task> {
        let mut store = self.lock().await;
        store.cancel(id)
    }

    async fn insert_message(&self, message: &Message) {
        let mut store = self.lock().await;
        store.insert_message(message);
    }
}

#[async_trait]
impl TaskEventRecorder for Mutex<TaskStore> {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Option<TaskUpdateEvent> {
        let mut store = self.lock().await;
        store.record_status_update(task_id, context_id, status)
    }

    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Option<TaskUpdateEvent> {
        let mut store = self.lock().await;
        store.record_artifact_update(task_id, context_id, artifact, append, last_chunk)
    }
}

#[async_trait]
impl TaskUpdateQueue for Mutex<TaskStore> {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        let mut store = self.lock().await;
        store.drain_updates(task_id)
    }
}

pub struct ProvenanceTaskStore {
    inner: Mutex<TaskStore>,
    writer: Option<Arc<dyn ProvenanceWriter>>,
    agent_id: AgentId,
}

impl ProvenanceTaskStore {
    pub fn new(writer: Option<Arc<dyn ProvenanceWriter>>, agent_id: AgentId) -> Self {
        Self {
            inner: Mutex::new(TaskStore::new()),
            writer,
            agent_id,
        }
    }

    async fn record_event(&self, event: ProvEvent) {
        if let Some(writer) = &self.writer {
            writer
                .add_event_with_logging(event, "task store operation")
                .await;
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn require_context_id(context_id: Option<ContextId>, operation: &str) -> ContextId {
    context_id.unwrap_or_else(|| {
        panic!(
            "context_id is required for {} in ProvenanceTaskStore; refusing implicit generation",
            operation
        )
    })
}

#[async_trait]
impl TaskRepository for ProvenanceTaskStore {
    async fn upsert(&self, mut task: Task) -> Option<Task> {
        let start = Instant::now();
        let context_id = require_context_id(task.context_id.clone(), "task upsert");

        // Always inject agent_id into task metadata from store-level agent_id
        if !task
            .metadata
            .as_ref()
            .is_some_and(|m| m.contains_key("agent_id"))
        {
            let mut metadata = task.metadata.unwrap_or_default();
            metadata.insert(
                "agent_id".to_string(),
                Value::String(self.agent_id.as_str().to_string()),
            );
            task.metadata = Some(metadata);
        }

        if let Some(task_id) = task.id.clone() {
            let event = ProvEvent::task_created(context_id, task_id, self.agent_id.clone());
            self.record_event(event).await;
        }
        let mut store = self.inner.lock().await;
        let out = store.upsert(task);
        metrics::record_task_store_operation("upsert", "success", start.elapsed());
        out
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let start = Instant::now();
        let store = self.inner.lock().await;
        let out = store.get(id, history_length);
        metrics::record_task_store_operation("get", "success", start.elapsed());
        out
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let start = Instant::now();
        let store = self.inner.lock().await;
        let out = store.list(request);
        metrics::record_task_store_operation("list", "success", start.elapsed());
        out
    }

    async fn cancel(&self, id: &str) -> Option<Task> {
        let mut store = self.inner.lock().await;
        store.cancel(id)
    }

    async fn insert_message(&self, message: &Message) {
        let context_id = require_context_id(message.context_id.clone(), "message insert");
        let task_id = message.task_id.clone();
        let role = message_role_string(&message.role);
        let content = message_content(message);
        tracing::trace!(
            context_id = context_id.as_str(),
            task_id = task_id.as_ref().map(|t| t.as_str()),
            message_id = message.message_id.as_message_id().as_str(),
            role = role.as_str(),
            content_parts = content.len(),
            has_writer = self.writer.is_some(),
            "Storing message in provenance",
        );

        // Always inject agent_id into message metadata from store-level agent_id
        let mut msg_metadata = message.metadata.clone();
        if !msg_metadata
            .as_ref()
            .is_some_and(|m| m.contains_key("agent_id"))
        {
            let mut metadata = msg_metadata.unwrap_or_default();
            metadata.insert(
                "agent_id".to_string(),
                Value::String(self.agent_id.as_str().to_string()),
            );
            msg_metadata = Some(metadata);
        }

        let metadata = msg_metadata.as_ref().map(metadata_string_map);

        // agent_id is always available from store level
        if let Some(task_id) = task_id.clone() {
            let event = ProvEvent::task_created(context_id.clone(), task_id, self.agent_id.clone());
            self.record_event(event).await;
        }
        let task_id_for_event = task_id.clone();

        let event = match (role.as_str(), task_id_for_event.clone()) {
            (ROLE_USER, Some(task_id)) => ProvEvent::message_received_task(
                context_id.clone(),
                task_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                now_millis(),
            ),
            (ROLE_USER, None) => ProvEvent::message_received_global(
                context_id.clone(),
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                now_millis(),
            ),
            (_, Some(task_id)) => ProvEvent::message_sent_task(
                context_id.clone(),
                task_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                now_millis(),
            ),
            (_, None) => ProvEvent::message_sent_global(
                context_id.clone(),
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                now_millis(),
            ),
        };
        self.record_event(event).await;

        let start = Instant::now();
        let mut store = self.inner.lock().await;
        store.insert_message(message);
        metrics::record_task_store_operation("insert_message", "success", start.elapsed());
    }
}

fn message_role_string(role: &MessageRole) -> String {
    match role {
        MessageRole::String(value) => value.clone(),
        MessageRole::Integer(value) => value.to_string(),
    }
}

fn message_content(message: &Message) -> Vec<String> {
    message
        .parts
        .iter()
        .filter_map(|part| part.text.clone())
        .collect()
}

fn metadata_string_map(metadata: &HashMap<String, Value>) -> HashMap<String, String> {
    metadata
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
        .collect()
}

#[async_trait]
impl TaskEventRecorder for ProvenanceTaskStore {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Option<TaskUpdateEvent> {
        if let Some(task_id) = task_id.clone() {
            let context_id = require_context_id(context_id.clone(), "status update");
            let event = ProvEvent::task_status_changed(
                context_id,
                task_id,
                None,
                status_to_string(&status),
            );
            self.record_event(event).await;
        }
        let mut store = self.inner.lock().await;
        store.record_status_update(task_id, context_id, status)
    }

    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Option<TaskUpdateEvent> {
        if let Some(task_id) = task_id.clone() {
            let context_id = require_context_id(context_id.clone(), "artifact update");
            let event = ProvEvent::task_artifact_generated(
                context_id,
                task_id,
                artifact.artifact_id.clone(),
                artifact.name.clone(),
            );
            self.record_event(event).await;
        }
        let mut store = self.inner.lock().await;
        store.record_artifact_update(task_id, context_id, artifact, append, last_chunk)
    }
}

#[async_trait]
impl TaskUpdateQueue for ProvenanceTaskStore {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        let mut store = self.inner.lock().await;
        store.drain_updates(task_id)
    }
}

fn status_to_string(status: &TaskStatus) -> Option<String> {
    status.state.as_ref().map(|state| match state {
        TaskState::String(value) => value.clone(),
        TaskState::Integer(value) => value.to_string(),
    })
}

impl TaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&mut self, task: Task) -> Option<Task> {
        let id = task.id.clone()?;
        let id_str = id.as_str();
        if !self.tasks.contains_key(id_str) {
            self.order.push(id_str.to_string());
        }
        self.tasks.insert(id_str.to_string(), task.clone());
        Some(task)
    }

    pub fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let mut task = self.tasks.get(id).cloned()?;
        if let Some(limit) = history_length {
            truncate_history(&mut task, limit);
        }
        Some(task)
    }

    pub fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let mut tasks: Vec<Task> = self
            .order
            .iter()
            .filter_map(|id| self.tasks.get(id).cloned())
            .collect();

        if let Some(context_id) = &request.context_id {
            tasks.retain(|task| {
                task.context_id.as_ref().map(|id| id.as_str()) == Some(context_id.as_str())
            });
        }

        if let Some(status) = &request.status {
            tasks.retain(|task| matches_task_state(task, status));
        }

        let include_artifacts = request.include_artifacts.unwrap_or(false);
        if !include_artifacts {
            for task in &mut tasks {
                task.artifacts.clear();
            }
        }

        if let Some(limit) = request
            .history_length
            .as_ref()
            .and_then(|value| value.as_usize())
        {
            for task in &mut tasks {
                truncate_history(task, limit);
            }
        }

        let total_size = tasks.len() as u64;
        let page_size = request
            .page_size
            .as_ref()
            .and_then(|value| value.as_usize())
            .unwrap_or(50);
        let start = request
            .page_token
            .as_ref()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let end = usize::min(start + page_size, tasks.len());

        let page_tasks = if start < tasks.len() {
            tasks[start..end].to_vec()
        } else {
            Vec::new()
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

    pub fn cancel(&mut self, id: &str) -> Option<Task> {
        let task = self.tasks.get_mut(id)?;
        let status = task.status.get_or_insert_with(TaskStatus::default);
        status.state = Some(TaskState::String(TASK_STATE_CANCELED.to_string()));
        Some(task.clone())
    }

    pub fn insert_message(&mut self, message: &Message) {
        if let Some(task_id) = &message.task_id
            && let Some(task) = self.tasks.get_mut(task_id.as_str())
        {
            task.history.push(message.clone());
        }
    }

    pub fn record_status_update(
        &mut self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Option<TaskUpdateEvent> {
        if let Some(task_id) = task_id {
            let task_id_str = task_id.as_str().to_string();
            let update = TaskStatusUpdateEvent {
                context_id,
                task_id: Some(task_id.clone()),
                status: Some(status),
                metadata: None,
                extra: HashMap::new(),
            };
            let event = TaskUpdateEvent::Status(update.clone());
            self.updates
                .entry(task_id_str)
                .or_default()
                .push(event.clone());
            return Some(event);
        }
        None
    }

    pub fn record_artifact_update(
        &mut self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Option<TaskUpdateEvent> {
        if let Some(task_id) = task_id {
            let task_id_str = task_id.as_str().to_string();
            let update = TaskArtifactUpdateEvent {
                context_id,
                task_id: Some(task_id.clone()),
                last_chunk,
                append,
                artifact: Some(artifact),
                metadata: None,
                extra: HashMap::new(),
            };
            let event = TaskUpdateEvent::Artifact(update.clone());
            self.updates
                .entry(task_id_str)
                .or_default()
                .push(event.clone());
            return Some(event);
        }
        None
    }

    pub fn drain_updates(&mut self, task_id: &str) -> Vec<TaskUpdateEvent> {
        self.updates.remove(task_id).unwrap_or_default()
    }
}

fn truncate_history(task: &mut Task, limit: usize) {
    if limit == 0 {
        task.history.clear();
        return;
    }
    if task.history.len() > limit {
        let start = task.history.len() - limit;
        task.history = task.history.split_off(start);
    }
}

fn matches_task_state(task: &Task, desired: &TaskState) -> bool {
    let Some(status) = &task.status else {
        return false;
    };
    let Some(state) = &status.state else {
        return false;
    };
    match (state, desired) {
        (TaskState::String(current), TaskState::String(target)) => current == target,
        (TaskState::Integer(current), TaskState::Integer(target)) => current == target,
        _ => false,
    }
}
