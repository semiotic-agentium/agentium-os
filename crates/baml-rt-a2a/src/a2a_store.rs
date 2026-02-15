use crate::a2a_types::{
    Artifact, ListTasksRequest, ListTasksResponse, Message, MessageRole, ROLE_USER,
    TASK_STATE_CANCELED, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};
use async_trait::async_trait;
use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use baml_rt_core::{BamlRtError, Result};
use baml_rt_observability::metrics;
use baml_rt_provenance::{ProvEvent, ProvenanceConversationContextItem, ProvenanceWriter};
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

    pub fn context_id(&self) -> Option<&ContextId> {
        match self {
            TaskUpdateEvent::Status(event) => event.context_id.as_ref(),
            TaskUpdateEvent::Artifact(event) => event.context_id.as_ref(),
        }
    }
}

#[derive(Debug, Default)]
pub struct TaskStore {
    tasks: HashMap<String, Task>,
    order: Vec<String>,
    updates: HashMap<String, Vec<TaskUpdateEvent>>,
}

/// Task persistence. Status transitions are **not** enforced on `upsert`; only
/// `TaskEventRecorder::record_status_update` applies the FSM. Callers must use
/// `record_status_update` for status changes; `upsert` is for task create/merge
/// (e.g. merge-preserve when status is `None`).
#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn upsert(&self, task: Task) -> Result<Option<Task>>;
    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task>;
    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse;
    async fn cancel(&self, id: &str) -> Option<Task>;
    async fn insert_message(&self, message: &Message) -> Result<()>;
}

#[async_trait]
pub trait TaskEventRecorder: Send + Sync {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>>;
    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>>;
}

#[async_trait]
pub trait TaskUpdateQueue: Send + Sync {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent>;
}

/// Atomic application of a stream chunk: task merge (status-none) + status + artifacts + message
/// in one critical section. Enforces I1 (FSM boundary) and I2 (atomic chunk apply).
#[async_trait]
pub trait TaskChunkApplier: Send + Sync {
    /// Applies task delta atomically. Task status is never applied via merge; status changes
    /// go through the FSM in `record_status_update`. Returns events for accepted updates.
    async fn apply_task_delta(
        &self,
        task: Option<Task>,
        message: Option<Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Result<Vec<TaskUpdateEvent>>;
}

/// Single source of truth for conversation context: read from the same store that
/// receives task/message updates. Unifies A2A task state and conversation; provenance
/// is the write-through audit log.
#[async_trait]
pub trait ConversationContextSource: Send + Sync {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;
}

#[async_trait]
pub trait TaskStoreBackend:
    TaskRepository + TaskEventRecorder + TaskUpdateQueue + TaskChunkApplier + ConversationContextSource
{
}

impl<T> TaskStoreBackend for T where
    T: TaskRepository
        + TaskEventRecorder
        + TaskUpdateQueue
        + TaskChunkApplier
        + ConversationContextSource
{
}

#[async_trait]
impl TaskRepository for Mutex<TaskStore> {
    async fn upsert(&self, task: Task) -> Result<Option<Task>> {
        let mut store = self.lock().await;
        Ok(store.upsert(task))
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

    async fn insert_message(&self, message: &Message) -> Result<()> {
        let mut store = self.lock().await;
        store.insert_message(message);
        Ok(())
    }
}

#[async_trait]
impl TaskEventRecorder for Mutex<TaskStore> {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let mut store = self.lock().await;
        Ok(store.record_status_update(task_id, context_id, status))
    }

    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let mut store = self.lock().await;
        Ok(store.record_artifact_update(task_id, context_id, artifact, append, last_chunk))
    }
}

#[async_trait]
impl TaskUpdateQueue for Mutex<TaskStore> {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        let mut store = self.lock().await;
        store.drain_updates(task_id)
    }
}

#[async_trait]
impl TaskChunkApplier for Mutex<TaskStore> {
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
        let mut store = self.lock().await;
        Ok(store.apply_task_delta(task, message, status_update, artifact_update))
    }
}

#[async_trait]
impl ConversationContextSource for Mutex<TaskStore> {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let store = self.lock().await;
        let messages = store.list_messages_in_context(context_id, limit);
        let items = messages
            .iter()
            .enumerate()
            .map(|(i, msg)| message_to_context_item(msg, i as u64 * 1000))
            .collect();
        Ok(items)
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

    fn inject_agent_id_into_chunk(
        task: Option<Task>,
        message: Option<Message>,
        agent_id: &AgentId,
    ) -> (Option<Task>, Option<Message>) {
        let task = task.map(|mut t| {
            ensure_agent_id_in_metadata(&mut t.metadata, agent_id);
            t
        });
        let message = message.map(|mut m| {
            ensure_agent_id_in_metadata(&mut m.metadata, agent_id);
            m
        });
        (task, message)
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "system time before UNIX_EPOCH, using 0 for provenance timestamp"
            );
            0
        })
}

fn require_context_id(context_id: Option<ContextId>, operation: &str) -> Result<ContextId> {
    context_id.ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "context_id is required for {} in ProvenanceTaskStore; refusing implicit generation",
            operation
        ))
    })
}

fn ensure_agent_id_in_metadata(metadata: &mut Option<HashMap<String, Value>>, agent_id: &AgentId) {
    if metadata
        .as_ref()
        .is_none_or(|m| !m.contains_key("agent_id"))
    {
        let mut m = metadata.take().unwrap_or_default();
        m.insert(
            "agent_id".to_string(),
            Value::String(agent_id.as_str().to_string()),
        );
        *metadata = Some(m);
    }
}

fn record_task_store_metrics(op: &str, outcome: &str, start: Instant) {
    metrics::record_task_store_operation(op, outcome, start.elapsed());
}

#[async_trait]
impl TaskRepository for ProvenanceTaskStore {
    async fn upsert(&self, mut task: Task) -> Result<Option<Task>> {
        let start = Instant::now();
        let context_id = require_context_id(task.context_id.clone(), "task upsert")?;

        ensure_agent_id_in_metadata(&mut task.metadata, &self.agent_id);

        if let Some(task_id) = task.id.clone() {
            let event = ProvEvent::task_created(context_id, task_id, self.agent_id.clone());
            self.record_event(event).await;
        }
        let mut store = self.inner.lock().await;
        let out = store.upsert(task);
        record_task_store_metrics("upsert", "success", start);
        Ok(out)
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let start = Instant::now();
        let store = self.inner.lock().await;
        let out = store.get(id, history_length);
        record_task_store_metrics("get", "success", start);
        out
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let start = Instant::now();
        let store = self.inner.lock().await;
        let out = store.list(request);
        record_task_store_metrics("list", "success", start);
        out
    }

    async fn cancel(&self, id: &str) -> Option<Task> {
        let out = {
            let mut store = self.inner.lock().await;
            store.cancel(id)
        };
        if let Some(ref task) = out
            && let (Some(cid), Some(tid)) = (task.context_id.clone(), task.id.clone())
            && let Ok(context_id) = require_context_id(Some(cid), "provenance cancel")
        {
            let event = ProvEvent::task_status_changed(
                context_id,
                tid,
                None,
                Some(TASK_STATE_CANCELED.to_string()),
            );
            self.record_event(event).await;
        }
        out
    }

    async fn insert_message(&self, message: &Message) -> Result<()> {
        let context_id = require_context_id(message.context_id.clone(), "message insert")?;
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

        let mut msg_metadata = message.metadata.clone();
        ensure_agent_id_in_metadata(&mut msg_metadata, &self.agent_id);
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
        record_task_store_metrics("insert_message", "success", start);
        Ok(())
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

fn message_to_context_item(
    message: &Message,
    timestamp_ms: u64,
) -> ProvenanceConversationContextItem {
    let role = message_role_string(&message.role);
    let content_parts = message_content(message);
    let content = Value::Array(
        content_parts
            .into_iter()
            .map(Value::String)
            .collect::<Vec<_>>(),
    );
    ProvenanceConversationContextItem {
        timestamp_ms,
        event_id: message.message_id.as_message_id().as_str().to_string(),
        role,
        content,
        source: "message".to_string(),
    }
}

fn metadata_string_map(metadata: &HashMap<String, Value>) -> HashMap<String, String> {
    metadata
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
        .collect()
}

#[async_trait]
impl TaskEventRecorder for ProvenanceTaskStore {
    /// I3: provenance emitted only after store accepts the status update.
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let start = Instant::now();
        let mut store = self.inner.lock().await;
        let out = store.record_status_update(task_id, context_id, status);
        let outcome = if out.is_some() { "success" } else { "rejected" };
        record_task_store_metrics("record_status_update", outcome, start);
        drop(store);
        if let Some(TaskUpdateEvent::Status(ref ev)) = out
            && let (Some(tid), Some(cid)) = (ev.task_id.as_ref(), ev.context_id.as_ref())
            && let Ok(context_id) = require_context_id(Some(cid.clone()), "provenance status")
        {
            let event = ProvEvent::task_status_changed(
                context_id,
                tid.clone(),
                None,
                ev.status.as_ref().and_then(status_to_string),
            );
            self.record_event(event).await;
        }
        Ok(out)
    }

    /// I3: provenance emitted only after store accepts the artifact update.
    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let start = Instant::now();
        let mut store = self.inner.lock().await;
        let out = store.record_artifact_update(task_id, context_id, artifact, append, last_chunk);
        let outcome = if out.is_some() { "success" } else { "rejected" };
        record_task_store_metrics("record_artifact_update", outcome, start);
        drop(store);
        if let Some(TaskUpdateEvent::Artifact(ref ev)) = out
            && let (Some(tid), Some(cid)) = (ev.task_id.as_ref(), ev.context_id.as_ref())
            && let Ok(context_id) = require_context_id(Some(cid.clone()), "provenance artifact")
        {
            let (aid, name) = ev
                .artifact
                .as_ref()
                .map(|a| (a.artifact_id.clone(), a.name.clone()))
                .unwrap_or((None, None));
            let event = ProvEvent::task_artifact_generated(context_id, tid.clone(), aid, name);
            self.record_event(event).await;
        }
        Ok(out)
    }
}

#[async_trait]
impl TaskUpdateQueue for ProvenanceTaskStore {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        let mut store = self.inner.lock().await;
        store.drain_updates(task_id)
    }
}

#[async_trait]
impl TaskChunkApplier for ProvenanceTaskStore {
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
        let task_created_prov = task.as_ref().and_then(|t| {
            let cid = t.context_id.clone()?;
            let tid = t.id.clone()?;
            Some((tid, cid))
        });
        let (task, message) = Self::inject_agent_id_into_chunk(task, message, &self.agent_id);
        let message_for_prov = message.clone();
        let start = Instant::now();
        let events = {
            let mut store = self.inner.lock().await;
            store.apply_task_delta(task, message, status_update, artifact_update)
        };
        record_task_store_metrics("apply_task_delta", "success", start);
        // I3: emit provenance only after store accepted the updates
        if let Some((tid, cid)) = task_created_prov
            && let Ok(context_id) = require_context_id(Some(cid), "provenance task_created")
        {
            let event = ProvEvent::task_created(context_id, tid, self.agent_id.clone());
            self.record_event(event).await;
        }
        for event in &events {
            if let Ok(context_id) =
                require_context_id(event.context_id().cloned(), "provenance after apply")
            {
                match event {
                    TaskUpdateEvent::Status(ev) => {
                        if let (Some(task_id), Some(status)) =
                            (ev.task_id.clone(), ev.status.as_ref())
                        {
                            let prov = ProvEvent::task_status_changed(
                                context_id,
                                task_id,
                                None,
                                status_to_string(status),
                            );
                            self.record_event(prov).await;
                        }
                    }
                    TaskUpdateEvent::Artifact(ev) => {
                        if let Some(task_id) = ev.task_id.clone() {
                            let (aid, name) = ev
                                .artifact
                                .as_ref()
                                .map(|a| (a.artifact_id.clone(), a.name.clone()))
                                .unwrap_or((None, None));
                            let prov =
                                ProvEvent::task_artifact_generated(context_id, task_id, aid, name);
                            self.record_event(prov).await;
                        }
                    }
                }
            }
        }
        // I3: emit MessageSent for agent reply when apply_task_delta receives a message chunk.
        // (User messages go through insert_message; agent messages from stream go through here.)
        if let Some(ref msg) = message_for_prov
            && let Ok(context_id) = require_context_id(
                msg.context_id.clone(),
                "provenance message in apply_task_delta",
            )
        {
            let role = message_role_string(&msg.role);
            let content = message_content(msg);
            let metadata = msg.metadata.as_ref().map(metadata_string_map);
            let event = match (role.as_str(), msg.task_id.clone()) {
                (ROLE_USER, _) => None,
                (_, Some(task_id)) => Some(ProvEvent::message_sent_task(
                    context_id,
                    task_id,
                    msg.message_id.as_message_id().clone(),
                    role,
                    content,
                    metadata,
                    now_millis(),
                )),
                (_, None) => Some(ProvEvent::message_sent_global(
                    context_id,
                    msg.message_id.as_message_id().clone(),
                    role,
                    content,
                    metadata,
                    now_millis(),
                )),
            };
            if let Some(prov) = event {
                self.record_event(prov).await;
            }
        }
        Ok(events)
    }
}

#[async_trait]
impl ConversationContextSource for ProvenanceTaskStore {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let store = self.inner.lock().await;
        let messages = store.list_messages_in_context(context_id, limit);
        let items = messages
            .iter()
            .enumerate()
            .map(|(i, msg)| message_to_context_item(msg, i as u64 * 1000))
            .collect();
        Ok(items)
    }
}

fn status_to_string(status: &TaskStatus) -> Option<String> {
    status.state.as_ref().map(|state| match state {
        TaskState::String(value) => value.clone(),
        TaskState::Integer(value) => value.to_string(),
    })
}

// FSM: terminal states and allowed transitions (A2A task lifecycle).
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

impl TaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or merges a task shell only. I1: status is never applied here; use
    /// `record_status_update` for status changes. Incoming `task.status` is ignored
    /// (stripped); merge-preserve: if existing has status, keep it; else leave None.
    pub fn upsert(&mut self, mut task: Task) -> Option<Task> {
        let id = task.id.clone()?;
        let id_str = id.as_str();
        if !self.tasks.contains_key(id_str) {
            self.order.push(id_str.to_string());
        }
        // I1: strip incoming status; merge-preserve existing status only.
        task.status = self
            .tasks
            .get(id_str)
            .and_then(|existing| existing.status.clone());
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

    /// Collects all messages in the given context from task histories (task order, then
    /// history order). Single source of truth for conversation; no separate provenance read.
    pub fn list_messages_in_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Vec<Message> {
        let context_str = context_id.as_str();
        let mut out: Vec<Message> = self
            .order
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .filter(|task| {
                task.context_id
                    .as_ref()
                    .is_some_and(|c| c.as_str() == context_str)
            })
            .flat_map(|task| task.history.iter().cloned())
            .collect();
        if let Some(n) = limit {
            if n == 0 {
                return Vec::new();
            }
            if out.len() > n {
                let start = out.len() - n;
                out = out.into_iter().skip(start).collect();
            }
        }
        out
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

    /// FSM-aware cancel: applies CANCELED via record_status_update so events and FSM are consistent.
    pub fn cancel(&mut self, id: &str) -> Option<Task> {
        let (task_id, context_id) = {
            let task = self.tasks.get(id)?;
            (task.id.clone()?, task.context_id.clone())
        };
        let status = TaskStatus {
            state: Some(TaskState::String(TASK_STATE_CANCELED.to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };
        self.record_status_update(Some(task_id), context_id, status)?;
        self.tasks.get(id).cloned()
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
        let task_id = task_id?;
        let task_id_str = task_id.as_str().to_string();
        let new_state = status_to_string(&status);
        let new_state = match &new_state {
            Some(s) => s.as_str(),
            None => return None,
        };

        let task = self.tasks.get_mut(&task_id_str)?;
        let current_state_str = task.status.as_ref().and_then(status_to_string);
        let current_state = current_state_str.as_deref();

        let allowed = match current_state {
            None => new_state == S_SUBMITTED,
            Some(current) if is_terminal_state(current) => false,
            Some(current) => is_allowed_transition(current, new_state),
        };
        if !allowed {
            return None;
        }

        task.status = Some(status.clone());
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
        Some(event)
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

    /// I2: Applies one stream chunk atomically (merge + status + artifacts + message).
    /// Task status is never applied via merge; status changes go through the FSM.
    pub fn apply_task_delta(
        &mut self,
        task: Option<Task>,
        message: Option<Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Vec<TaskUpdateEvent> {
        let mut out = Vec::new();
        if let Some(mut t) = task {
            let status = t.status.take();
            let context_id = t.context_id.clone();
            let task_id = t.id.clone();
            let artifacts = std::mem::take(&mut t.artifacts);
            let result = self.upsert(t);
            debug_assert!(
                result.is_some(),
                "apply_task_delta: task without id is a logic error"
            );
            if let Some(status) = status
                && let Some(ev) =
                    self.record_status_update(task_id.clone(), context_id.clone(), status)
            {
                out.push(ev);
            }
            if let Some(tid) = task_id {
                for artifact in artifacts {
                    if let Some(ev) = self.record_artifact_update(
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
            self.insert_message(&msg);
        }
        if let Some(ref up) = status_update
            && let Some(status) = up.status.clone()
            && let Some(ev) =
                self.record_status_update(up.task_id.clone(), up.context_id.clone(), status)
        {
            out.push(ev);
        }
        if let Some(ref up) = artifact_update
            && let Some(ev) = self.record_artifact_update(
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
