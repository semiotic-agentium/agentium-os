use std::{collections::HashMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::{
    BamlRtError, Citation, Result, clock_events,
    ids::{AgentId, ContextId, TaskId},
};
use baml_rt_observability::metrics;
use baml_rt_provenance::{ProvEvent, ProvenanceError, ProvenanceWriter, events::ReservedAnchor};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::a2a_types::{
    Artifact, ListTasksRequest, ListTasksResponse, Message, MessageRole, Part, ROLE_USER,
    TASK_STATE_CANCELED, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent, ValidatedTaskChunk,
};

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
    async fn ensure_task_exists(
        &self,
        task_id: &TaskId,
        context_id: Option<&ContextId>,
    ) -> Result<()>;
    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task>;
    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse;
    async fn cancel(&self, id: &str) -> Option<Task>;
    async fn insert_message(&self, message: &Message) -> Result<()>;

    /// Insert a received message (ROLE_USER) with explicit scope. Use this for all inbound
    /// messages so provenance cannot be misattributed to the sender's (context_id, task_id).
    /// Default: clones message with scope values and calls insert_message.
    async fn insert_message_for_receiver(
        &self,
        message: &Message,
        context_id: ContextId,
        task_id: Option<TaskId>,
    ) -> Result<()> {
        let mut msg = message.clone();
        msg.context_id = Some(context_id);
        msg.task_id = task_id;
        self.insert_message(&msg).await
    }
}

#[async_trait]
pub trait TaskEventRecorder: Send + Sync {
    async fn record_status_update(
        &self,
        task_id: TaskId,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>>;
    async fn record_artifact_update(
        &self,
        task_id: TaskId,
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
    async fn apply_task_chunk(&self, chunk: ValidatedTaskChunk) -> Result<Vec<TaskUpdateEvent>>;
}

/// Source of **BAML conversation rows** (`ProvenanceConversationContextItem`) for prompt projection.
///
/// Rows come **only** from the provenance graph ([`ProvenanceWriter::conversation_context`]).
/// Agent startup always mounts a [`ProvenanceWriter`] (file, explicit in-memory Surreal, or the
/// builder default in-memory graph). There is no history path without a database.
///
/// [`TaskStoreBackend`] deliberately does **not** include this trait — task/message mirrors are
/// for transport and lifecycle, not for reconstructing LLM-visible history.
///
/// For resume, the caller must use the same `context_id` as the session. A caller-provided
/// `limit` truncates in the provenance reader; the A2A prompt path uses
/// [`baml_rt_provenance::DEFAULT_LLM_CONTEXT_ITEM_CAP`] as the default cap.
#[async_trait]
pub trait ConversationContextSource: Send + Sync {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;
}

/// Conversation context read **only** from the provenance writer / graph (e.g. SurrealDB).
///
/// Wired into the BAML runtime for every agent build: one projection, one store.
#[derive(Clone)]
pub struct ProvenanceWriterConversationSource {
    writer: Arc<dyn ProvenanceWriter>,
}

impl ProvenanceWriterConversationSource {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl ConversationContextSource for ProvenanceWriterConversationSource {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.writer
            .conversation_context(context_id, limit)
            .await
            .map_err(|source| BamlRtError::ProvenanceContextRead {
                source: Box::new(source),
            })
    }
}

#[async_trait]
pub trait TaskStoreBackend:
    TaskRepository + TaskEventRecorder + TaskUpdateQueue + TaskChunkApplier
{
}

impl<T> TaskStoreBackend for T where
    T: TaskRepository + TaskEventRecorder + TaskUpdateQueue + TaskChunkApplier
{
}

#[async_trait]
impl TaskRepository for Mutex<TaskStore> {
    async fn upsert(&self, task: Task) -> Result<Option<Task>> {
        let mut store = self.lock().await;
        Ok(store.upsert(task))
    }

    async fn ensure_task_exists(
        &self,
        task_id: &TaskId,
        context_id: Option<&ContextId>,
    ) -> Result<()> {
        let mut store = self.lock().await;
        store.ensure_task_exists(task_id.clone(), context_id.cloned());
        Ok(())
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
        task_id: TaskId,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let mut store = self.lock().await;
        Ok(store.record_status_update(Some(task_id), context_id, status))
    }

    async fn record_artifact_update(
        &self,
        task_id: TaskId,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let mut store = self.lock().await;
        Ok(store.record_artifact_update(Some(task_id), context_id, artifact, append, last_chunk))
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
    async fn apply_task_chunk(&self, chunk: ValidatedTaskChunk) -> Result<Vec<TaskUpdateEvent>> {
        let mut store = self.lock().await;
        Ok(store.apply_task_chunk(&chunk))
    }
}

pub struct ProvenanceTaskStore {
    inner: Arc<dyn TaskStoreBackend>,
    /// Required: all task/message side effects are mirrored into this graph for audit and for
    /// [`ProvenanceWriter::conversation_context`]. There is no task-history path for LLM-visible rows.
    writer: Arc<dyn ProvenanceWriter>,
    agent_id: AgentId,
}

impl ProvenanceTaskStore {
    /// In-memory task mirror with a provenance writer (conversation context is **only** via the graph).
    pub fn new(writer: Arc<dyn ProvenanceWriter>, agent_id: AgentId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TaskStore::new())),
            writer,
            agent_id,
        }
    }

    /// Task store backed by a persistent backend (e.g. [crate::task_subgraph_store::TaskSubgraphStore])
    /// with the same provenance writer used for graph writes and conversation reads.
    pub fn with_backend(
        inner: Arc<dyn TaskStoreBackend>,
        writer: Arc<dyn ProvenanceWriter>,
        agent_id: AgentId,
    ) -> Self {
        Self {
            inner,
            writer,
            agent_id,
        }
    }

    async fn record_event(&self, event: ProvEvent) {
        self.writer
            .add_event_with_logging(event, "task store operation")
            .await;
    }

    /// Emit a `TaskStatusChanged` provenance event using a pre-allocated anchor, then
    /// conditionally emit `TaskExecutionEnded` for terminal states.
    ///
    /// The `anchor` should be a [`ReservedAnchor`] allocated **before** any `async` await
    /// that precedes this call, to preserve logical `event_order` under concurrency.
    async fn emit_status_provenance(
        &self,
        anchor: ReservedAnchor,
        context_id: baml_rt_core::ids::ContextId,
        task_id: baml_rt_core::ids::TaskId,
        status_str: Option<String>,
    ) {
        let event = ProvEvent::task_status_changed_with_id(
            anchor,
            context_id.clone(),
            task_id.clone(),
            None,
            status_str.clone(),
        );
        self.record_event(event).await;
        if status_str.as_deref().is_some_and(is_terminal_state) {
            self.record_event(ProvEvent::task_execution_ended(context_id, task_id))
                .await;
        }
    }

    async fn record_event_required(&self, event: ProvEvent, context: &str) -> Result<()> {
        self.writer
            .add_event(event)
            .await
            .map_err(|source| match source {
                // Bounded write contention is host-retriable, not LLM-correctable —
                // it means concurrent writers raced on a shared graph record (agent
                // runtime instance, context entity), not that the caller's input
                // was malformed. The HostRetriable disposition lets clients re-queue.
                ProvenanceError::Contention { ref details } => BamlRtError::Conflict(format!(
                    "provenance write contention for {context}: {details}"
                )),
                other => BamlRtError::InvalidArgumentWithSource {
                    message: format!("failed to record provenance event for {context}"),
                    source: Box::new(other),
                },
            })?;
        Ok(())
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

    /// Insert received message using explicit scope. Never reads context_id/task_id from message.
    /// Caller must pass ROLE_USER messages only.
    async fn insert_message_with_scope(
        &self,
        message: &Message,
        context_id: ContextId,
        task_id: Option<TaskId>,
    ) -> Result<()> {
        let role = message_role_string(&message.role);
        if role != ROLE_USER {
            return Err(BamlRtError::InvalidArgument(format!(
                "insert_message_with_scope requires ROLE_USER, got {role}"
            )));
        }
        let content = validated_message_content(message, "insert_message_with_scope")?;
        tracing::trace!(
            context_id = context_id.as_str(),
            task_id = task_id.as_ref().map(|t| t.as_str()),
            message_id = message.message_id.as_message_id().as_str(),
            role = role.as_str(),
            "Storing received message with explicit scope (no wire-derived context/task)",
        );
        let mut msg_metadata = message.metadata.clone();
        ensure_agent_id_in_metadata(&mut msg_metadata, &self.agent_id);
        let metadata = msg_metadata.as_ref().map(metadata_string_map);
        if let Some(ref tid) = task_id {
            self.record_event_required(
                ProvEvent::task_exists(context_id.clone(), tid.clone()),
                "insert_message_with_scope task_exists",
            )
            .await?;
            self.record_event_required(
                ProvEvent::task_execution_started(
                    context_id.clone(),
                    tid.clone(),
                    self.agent_id.clone(),
                ),
                "insert_message_with_scope task_execution_started",
            )
            .await?;
        }
        let event = match task_id.clone() {
            Some(tid) => ProvEvent::message_received_task(
                context_id.clone(),
                tid,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                now_millis(),
            ),
            None => ProvEvent::message_received_global(
                context_id.clone(),
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                now_millis(),
            ),
        };
        self.record_event_required(event, "insert_message_with_scope message lifecycle")
            .await?;
        if task_id.is_some() {
            let mut msg = message.clone();
            msg.context_id = Some(context_id);
            msg.task_id = task_id;
            let start = Instant::now();
            self.inner.insert_message(&msg).await?;
            record_task_store_metrics("insert_message_with_scope", "success", start);
        }
        Ok(())
    }
}

fn now_millis() -> u64 {
    baml_rt_core::now_unix_ms(clock_events::A2A_STORE)
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
            self.record_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
                .await;
            self.record_event(ProvEvent::task_execution_started(
                context_id,
                task_id,
                self.agent_id.clone(),
            ))
            .await;
        }
        let out = self.inner.upsert(task).await?;
        record_task_store_metrics("upsert", "success", start);
        Ok(out)
    }

    async fn ensure_task_exists(
        &self,
        task_id: &TaskId,
        context_id: Option<&ContextId>,
    ) -> Result<()> {
        let start = Instant::now();
        self.inner.ensure_task_exists(task_id, context_id).await?;
        record_task_store_metrics("ensure_task_exists", "success", start);
        Ok(())
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let start = Instant::now();
        let out = self.inner.get(id, history_length).await;
        record_task_store_metrics("get", "success", start);
        out
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let start = Instant::now();
        let out = self.inner.list(request).await;
        record_task_store_metrics("list", "success", start);
        out
    }

    async fn cancel(&self, id: &str) -> Option<Task> {
        // Reserve anchor before the await — same race as record_status_update / apply_task_delta.
        let prov_anchor = ReservedAnchor::allocate();
        let out = self.inner.cancel(id).await;
        if let Some(ref task) = out
            && let (Some(cid), Some(tid)) = (task.context_id.clone(), task.id.clone())
            && let Ok(context_id) = require_context_id(Some(cid), "provenance cancel")
        {
            self.emit_status_provenance(
                prov_anchor,
                context_id,
                tid,
                Some(TASK_STATE_CANCELED.to_string()),
            )
            .await;
        }
        out
    }

    async fn insert_message_for_receiver(
        &self,
        message: &Message,
        context_id: ContextId,
        task_id: Option<TaskId>,
    ) -> Result<()> {
        // Provenance: use explicit scope only; never read context_id/task_id from message.
        self.insert_message_with_scope(message, context_id, task_id)
            .await
    }

    async fn insert_message(&self, message: &Message) -> Result<()> {
        let context_id = require_context_id(message.context_id.clone(), "message insert")?;
        let task_id = message.task_id.clone();
        let role = message_role_string(&message.role);
        let content = validated_message_content(message, "insert_message")?;
        tracing::trace!(
            context_id = context_id.as_str(),
            task_id = task_id.as_ref().map(|t| t.as_str()),
            message_id = message.message_id.as_message_id().as_str(),
            role = role.as_str(),
            content_parts = content.len(),
            "Storing message in provenance",
        );

        let mut msg_metadata = message.metadata.clone();
        ensure_agent_id_in_metadata(&mut msg_metadata, &self.agent_id);
        let metadata = msg_metadata.as_ref().map(metadata_string_map);

        // agent_id is always available from store level
        if let Some(task_id) = task_id.clone() {
            self.record_event_required(
                ProvEvent::task_exists(context_id.clone(), task_id.clone()),
                "insert_message task_exists",
            )
            .await?;
            self.record_event_required(
                ProvEvent::task_execution_started(
                    context_id.clone(),
                    task_id,
                    self.agent_id.clone(),
                ),
                "insert_message task_execution_started",
            )
            .await?;
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
                self.agent_id.clone(),
                now_millis(),
            ),
            (ROLE_USER, None) => ProvEvent::message_received_global(
                context_id.clone(),
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                now_millis(),
            ),
            (_, Some(task_id)) => ProvEvent::message_sent_task(
                context_id.clone(),
                task_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                now_millis(),
                Vec::new(),
            ),
            (_, None) => ProvEvent::message_sent_global(
                context_id.clone(),
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                now_millis(),
                Vec::new(),
            ),
        };
        self.record_event_required(event, "insert_message message lifecycle")
            .await?;

        // Task-scoped backends (e.g. TaskSubgraphStore) require task_id; global
        // messages are only in the provenance event stream and appear in context_messages.
        if message.task_id.is_some() {
            let start = Instant::now();
            self.inner.insert_message(message).await?;
            record_task_store_metrics("insert_message", "success", start);
        }
        Ok(())
    }
}

pub(crate) fn message_role_string(role: &MessageRole) -> String {
    role.as_wire_str().to_string()
}

/// Maximum UTF-8 bytes for JSON / raw previews embedded in provenance and conversation history.
const STRUCTURED_PREVIEW_MAX_BYTES: usize = 8192;

fn truncate_utf8_owned(mut s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes.saturating_sub(1);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

/// Compact JSON for structured-data parts — replaces the old `[structured-data part]` placeholder
/// so UIs and transcripts show actual payload (truncated when huge).
fn json_preview_for_provenance(value: &Value) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "\"<non-serializable>\"".to_string());
    truncate_utf8_owned(s, STRUCTURED_PREVIEW_MAX_BYTES)
}

fn raw_preview_for_provenance(raw: &str) -> String {
    truncate_utf8_owned(raw.to_string(), STRUCTURED_PREVIEW_MAX_BYTES)
}

fn file_part_preview(part: &Part) -> String {
    let mut bits: Vec<String> = Vec::new();
    if let Some(ref u) = part.url {
        bits.push(format!("url={u}"));
    }
    if let Some(ref f) = part.filename {
        bits.push(format!("file={f}"));
    }
    if bits.is_empty() {
        "[file part]".to_string()
    } else {
        format!("[file part] {}", bits.join(" "))
    }
}

/// Provenance-safe content extraction from A2A message parts.
///
/// Text parts are included verbatim (trimmed, blanks dropped). Structured-data parts
/// contribute compact JSON previews (truncated when large). File and raw parts include
/// short descriptors so conversation history remains human-readable.
pub(crate) fn validated_message_content(
    message: &Message,
    _operation: &str,
) -> Result<Vec<String>> {
    let mut content: Vec<String> = Vec::new();
    for part in &message.parts {
        if let Some(ref text) = part.text {
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                content.push(trimmed);
            }
            continue;
        }
        if let Some(ref data) = part.data {
            content.push(json_preview_for_provenance(data));
            continue;
        }
        if part.url.is_some() || part.filename.is_some() {
            content.push(file_part_preview(part));
            continue;
        }
        if let Some(ref raw) = part.raw {
            content.push(raw_preview_for_provenance(raw));
            continue;
        }
        // Parts with none of the above fields are truly empty — skip them.
    }
    Ok(content)
}

pub(crate) fn metadata_string_map(metadata: &HashMap<String, Value>) -> HashMap<String, String> {
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
        task_id: TaskId,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let start = Instant::now();
        // Reserve the activity anchor BEFORE the inner await so that event_order is
        // assigned in logical emission order, not in DB-round-trip completion order.
        // (Two tokio tasks can race here: the QuickJS task emits WORKING via this path
        // while the drain-loop task emits COMPLETED via apply_task_delta.)
        let prov_anchor = ReservedAnchor::allocate();
        let out = self
            .inner
            .record_status_update(task_id, context_id, status)
            .await?;
        let outcome = if out.is_some() { "success" } else { "rejected" };
        record_task_store_metrics("record_status_update", outcome, start);
        if let Some(TaskUpdateEvent::Status(ref ev)) = out
            && let (Some(tid), Some(cid)) = (ev.task_id.as_ref(), ev.context_id.as_ref())
            && let Ok(context_id) = require_context_id(Some(cid.clone()), "provenance status")
        {
            self.emit_status_provenance(
                prov_anchor,
                context_id,
                tid.clone(),
                ev.status.as_ref().and_then(status_to_string),
            )
            .await;
        }
        Ok(out)
    }

    /// I3: provenance emitted only after store accepts the artifact update.
    async fn record_artifact_update(
        &self,
        task_id: TaskId,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let start = Instant::now();
        let out = self
            .inner
            .record_artifact_update(task_id, context_id, artifact, append, last_chunk)
            .await?;
        let outcome = if out.is_some() { "success" } else { "rejected" };
        record_task_store_metrics("record_artifact_update", outcome, start);
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
        self.inner.drain_updates(task_id).await
    }
}

#[async_trait]
impl TaskChunkApplier for ProvenanceTaskStore {
    async fn apply_task_chunk(&self, chunk: ValidatedTaskChunk) -> Result<Vec<TaskUpdateEvent>> {
        let new_task_prov = chunk.task().and_then(|t| {
            let cid = t.context_id.clone()?;
            let tid = t.id.clone()?;
            Some((tid, cid))
        });
        let mut sr = chunk.into_stream_response();
        let (task, message) =
            Self::inject_agent_id_into_chunk(sr.task.take(), sr.message.take(), &self.agent_id);
        sr.task = task;
        sr.message = message;
        let chunk = ValidatedTaskChunk::try_from(sr)?;
        let message_for_prov = chunk.message().cloned();
        // Reserve an activity anchor BEFORE the inner await so that event_order is assigned
        // in logical emission order, not DB-round-trip completion order.
        // The drain-loop task (COMPLETED) and the QuickJS task (WORKING) race through here
        // concurrently; one pre-reserved anchor suffices for the typical single status
        // transition per chunk. Additional transitions take fresh anchors (rare).
        let mut status_anchor = Some(ReservedAnchor::allocate());
        let start = Instant::now();
        let events = self.inner.apply_task_chunk(chunk).await?;
        record_task_store_metrics("apply_task_chunk", "success", start);
        // I3: emit provenance only after store accepted the updates
        if let Some((tid, cid)) = new_task_prov
            && let Ok(context_id) = require_context_id(Some(cid), "provenance task_exists")
        {
            self.record_event(ProvEvent::task_exists(context_id.clone(), tid.clone()))
                .await;
            self.record_event(ProvEvent::task_execution_started(
                context_id,
                tid,
                self.agent_id.clone(),
            ))
            .await;
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
                            // Consume the pre-reserved anchor on first use; allocate fresh
                            // ones for subsequent status events in the same chunk.
                            let anchor = status_anchor
                                .take()
                                .unwrap_or_else(ReservedAnchor::allocate);
                            self.emit_status_provenance(
                                anchor,
                                context_id,
                                task_id,
                                status_to_string(status),
                            )
                            .await;
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
        // I3: emit message lifecycle events for any chunk message accepted by the store.
        if let Some(ref msg) = message_for_prov {
            let context_id = require_context_id(
                msg.context_id.clone(),
                "provenance message in apply_task_chunk",
            )?;
            let role = message_role_string(&msg.role);
            let content = validated_message_content(msg, "apply_task_chunk")?;
            let metadata = msg.metadata.as_ref().map(metadata_string_map);
            // Extract validated citation refs from wire metadata before the lossy string-map
            // conversion drops the array. Citations are model-produced #N/@N ref strings.
            let citations: Vec<Citation> = msg
                .metadata
                .as_ref()
                .and_then(|m| m.get("citations"))
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .filter_map(|s| match Citation::try_new(s) {
                            Ok(c) => Some(c),
                            Err(e) => {
                                tracing::warn!(raw = s, error = %e, "citation parse failed; skipping");
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let event = match (role.as_str(), msg.task_id.clone()) {
                (ROLE_USER, Some(task_id)) => ProvEvent::message_received_task(
                    context_id,
                    task_id,
                    msg.message_id.as_message_id().clone(),
                    role,
                    content,
                    metadata,
                    self.agent_id.clone(),
                    now_millis(),
                ),
                (ROLE_USER, None) => ProvEvent::message_received_global(
                    context_id,
                    msg.message_id.as_message_id().clone(),
                    role,
                    content,
                    metadata,
                    self.agent_id.clone(),
                    now_millis(),
                ),
                (_, Some(task_id)) => ProvEvent::message_sent_task(
                    context_id,
                    task_id,
                    msg.message_id.as_message_id().clone(),
                    role,
                    content,
                    metadata,
                    self.agent_id.clone(),
                    now_millis(),
                    citations,
                ),
                (_, None) => ProvEvent::message_sent_global(
                    context_id,
                    msg.message_id.as_message_id().clone(),
                    role,
                    content,
                    metadata,
                    self.agent_id.clone(),
                    now_millis(),
                    citations,
                ),
            };
            self.record_event_required(event, "apply_task_chunk message lifecycle")
                .await?;
        }
        Ok(events)
    }
}

pub(crate) fn status_to_string(status: &TaskStatus) -> Option<String> {
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

/// Drop `message.taskId` on `message.sendStream` when the
/// referenced task is absent from the store or already reached a terminal lifecycle state. Keeping a
/// stale pin after `TASK_STATE_*` completion makes the host treat the turn like a task resume and
/// breaks the next conversational hop under the same `contextId`; clients should not need special
/// cases if the transport normalizes here.
pub(crate) fn should_strip_wire_task_id_for_message_send_stream(task: Option<&Task>) -> bool {
    match task {
        None => true,
        Some(t) => {
            let Some(st) = t.status.as_ref().and_then(status_to_string) else {
                return true;
            };
            is_terminal_state(st.as_str())
        }
    }
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

    /// Insert a minimal task shell only when absent; never overwrites existing fields.
    pub fn ensure_task_exists(&mut self, task_id: TaskId, context_id: Option<ContextId>) {
        let id = task_id.as_str().to_string();
        if self.tasks.contains_key(&id) {
            return;
        }
        self.order.push(id.clone());
        self.tasks.insert(
            id,
            Task {
                id: Some(task_id),
                context_id,
                artifacts: Vec::new(),
                history: Vec::new(),
                status: None,
                metadata: None,
                extra: HashMap::new(),
            },
        );
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
        let new_state = new_state.as_ref()?.as_str();

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
    pub fn apply_task_chunk(&mut self, chunk: &ValidatedTaskChunk) -> Vec<TaskUpdateEvent> {
        crate::task_chunk_apply::apply_validated_chunk_to_task_store(self, chunk)
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

#[cfg(test)]
mod strip_wire_task_id_tests {
    use std::collections::HashMap;

    use super::{Task, TaskState, TaskStatus, should_strip_wire_task_id_for_message_send_stream};

    fn task_with_state(state: &str) -> Task {
        Task {
            id: None,
            context_id: None,
            artifacts: Vec::new(),
            history: Vec::new(),
            status: Some(TaskStatus {
                state: Some(TaskState::String(state.to_string())),
                message: None,
                timestamp: None,
                extra: HashMap::new(),
            }),
            metadata: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn strip_when_task_unknown() {
        assert!(should_strip_wire_task_id_for_message_send_stream(None));
    }

    #[test]
    fn strip_when_status_missing() {
        let task = Task {
            id: None,
            context_id: None,
            artifacts: Vec::new(),
            history: Vec::new(),
            status: None,
            metadata: None,
            extra: HashMap::new(),
        };
        assert!(should_strip_wire_task_id_for_message_send_stream(Some(
            &task
        )));
    }

    #[test]
    fn strip_when_terminal_completed() {
        assert!(should_strip_wire_task_id_for_message_send_stream(Some(
            &task_with_state("TASK_STATE_COMPLETED",)
        )));
    }

    #[test]
    fn keep_when_input_required() {
        assert!(!should_strip_wire_task_id_for_message_send_stream(Some(
            &task_with_state("TASK_STATE_INPUT_REQUIRED",)
        )));
    }

    #[test]
    fn keep_when_working() {
        assert!(!should_strip_wire_task_id_for_message_send_stream(Some(
            &task_with_state("TASK_STATE_WORKING",)
        )));
    }
}
