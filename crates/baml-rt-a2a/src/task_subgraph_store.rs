//! A2A task store backed entirely by the provenance graph.
//!
//! Reads route through the typed [`TaskGraphReader`] surface
//! ([`TaskGraphReader::hydrate`] /
//! [`TaskGraphReader::hydrate_batch`] /
//! [`TaskGraphReader::latest_in_context`]); writes route through
//! [`ProvenanceWriter::add_event`] using the canonical
//! [`ProvEvent::task_status_changed`] /
//! [`ProvEvent::task_artifact_generated`] constructors. The previous
//! relational mirror tables (`a2a_task` / `a2a_message` /
//! `a2a_update`) are no longer touched.
//!
//! Live task-update delivery (replacing the durable `a2a_update` SSE
//! replay queue) flows through the in-memory
//! [`TaskUpdateBroadcaster`]; durable replay-on-reconnect uses
//! [`TaskGraphReader::replay_since`].

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result, clock_events,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_provenance::{
    ProvenanceWriter, TaskGraphReader, events::ProvEvent, metamodel::ScopedTaskRef,
};

use crate::{
    a2a_store::{
        TaskChunkApplier, TaskEventRecorder, TaskRepository, TaskUpdateEvent,
        transcript_text_from_wire_status_message, wire_status_to_kind,
    },
    a2a_types::{
        Artifact, ListTasksRequest, ListTasksResponse, Message, Part, Task,
        TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent, ValidatedTaskChunk,
    },
    task_update_broadcaster::{ArtifactRef, TaskStreamKey, TaskUpdateBroadcaster, TaskUpdateFrame},
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

/// Recover a wire `ContextId` from the on-disk
/// `"context:<context_id>"` encoding used by the normalizer (mirror of
/// [`baml_rt_provenance::SurrealProvenanceStore`]'s
/// `context_id_from_node_id` helper, kept here so we don't have to
/// expose the encoding through `ScopedTaskRef`).
fn context_id_from_node_id(raw: &str) -> ContextId {
    let stripped = raw.strip_prefix("context:").unwrap_or(raw);
    ContextId::parse_temporal(stripped).unwrap_or_else(|| ContextId::from(stripped))
}

fn status_is_input_required(status: &TaskStatus) -> bool {
    matches!(
        &status.state,
        Some(TaskState::String(s)) if s == S_INPUT_REQUIRED
    )
}

fn input_required_transcript_message(
    task_id: &TaskId,
    context_id: &ContextId,
    status: &TaskStatus,
) -> Option<Message> {
    if !status_is_input_required(status) {
        return None;
    }
    let wire = status.message.as_ref()?;
    let text = transcript_text_from_wire_status_message(wire)?;
    let mid = ir_transcript_external_id(task_id, &text);
    Some(Message {
        message_id: crate::a2a_types::A2aMessageId::incoming(mid),
        role: crate::a2a_types::MessageRole::Agent,
        parts: vec![Part {
            text: Some(text),
            ..Default::default()
        }],
        context_id: Some(context_id.clone()),
        task_id: Some(task_id.clone()),
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    })
}

fn prompt_to_status_message(task_id: &TaskId, context_id: &ContextId, prompt: &str) -> Message {
    Message {
        message_id: crate::a2a_types::A2aMessageId::incoming(ir_transcript_external_id(
            task_id, prompt,
        )),
        role: crate::a2a_types::MessageRole::Agent,
        parts: vec![Part {
            text: Some(prompt.to_string()),
            ..Default::default()
        }],
        context_id: Some(context_id.clone()),
        task_id: Some(task_id.clone()),
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

fn ir_transcript_external_id(task_id: &TaskId, prompt: &str) -> ExternalId {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };
    let mut h = DefaultHasher::new();
    task_id.as_str().hash(&mut h);
    prompt.hash(&mut h);
    ExternalId::new(format!("{}-ir-{:x}", task_id.as_str(), h.finish()))
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

/// Graph-only A2A task store. Combines a [`TaskGraphReader`] (read
/// surface), a [`ProvenanceWriter`] (write surface — `add_event`
/// emissions), and an optional [`TaskUpdateBroadcaster`] (live SSE
/// notification surface).
///
/// The broadcaster is optional: callers running in tests or
/// integrations that do not host SSE subscribers may pass [`None`];
/// the store falls back to a dedicated default broadcaster so writes
/// remain idempotent.
pub struct TaskSubgraphStore {
    reader: Arc<dyn TaskGraphReader>,
    writer: Arc<dyn ProvenanceWriter>,
    broadcaster: Arc<TaskUpdateBroadcaster>,
}

impl TaskSubgraphStore {
    pub fn new(reader: Arc<dyn TaskGraphReader>, writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self {
            reader,
            writer,
            broadcaster: Arc::new(TaskUpdateBroadcaster::default()),
        }
    }

    pub fn with_broadcaster(
        reader: Arc<dyn TaskGraphReader>,
        writer: Arc<dyn ProvenanceWriter>,
        broadcaster: Arc<TaskUpdateBroadcaster>,
    ) -> Self {
        Self {
            reader,
            writer,
            broadcaster,
        }
    }

    pub fn broadcaster(&self) -> Arc<TaskUpdateBroadcaster> {
        self.broadcaster.clone()
    }

    fn map_writer_err(source: baml_rt_provenance::ProvenanceError) -> BamlRtError {
        BamlRtError::ProvenanceContextRead {
            source: Box::new(source),
        }
    }

    /// Resolve the canonical `ScopedTaskRef` for `(ctx, tid)`. Returns
    /// `None` when either the task does not exist or it is not
    /// `SCOPED_TO` `ctx` (cross-context forgery is structurally
    /// rejected by the typestate).
    async fn scoped(&self, ctx: &ContextId, tid: &TaskId) -> Result<Option<ScopedTaskRef>> {
        self.reader
            .resolve_scoped(ctx, tid)
            .await
            .map_err(Self::map_writer_err)
    }
}

/// Convert a hydrated graph view into the wire-shaped
/// [`crate::a2a_types::Task`]. The historical `metadata` / `extra`
/// fields are deliberately dropped because the graph-backed task view
/// does not model them as typed provenance data.
fn hydrated_to_wire_task(
    h: baml_rt_provenance::HydratedTask,
    history_messages: Vec<Message>,
) -> Task {
    let status = h
        .status
        .as_ref()
        .map(|state| status_kind_to_wire_status(&h.task_id, &h.context_id, state));
    let artifacts = h
        .artifacts
        .into_iter()
        .map(|aref| Artifact {
            artifact_id: aref.artifact_id,
            name: None,
            description: None,
            parts: Vec::new(),
            extensions: Vec::new(),
            metadata: aref.artifact_type.map(|t| {
                let mut map = HashMap::new();
                map.insert("artifact_type".to_string(), serde_json::Value::String(t));
                map
            }),
            extra: HashMap::new(),
        })
        .collect();
    Task {
        id: Some(h.task_id),
        context_id: Some(h.context_id),
        artifacts,
        history: history_messages,
        status,
        metadata: None,
        extra: HashMap::new(),
    }
}

fn status_kind_to_wire_status(
    task_id: &TaskId,
    context_id: &ContextId,
    state: &baml_rt_provenance::metamodel::A2ATaskStateProps,
) -> TaskStatus {
    let mut status = TaskStatus {
        state: Some(TaskState::String(
            state.new_status.as_wire_str().to_string(),
        )),
        message: None,
        timestamp: Some(state.transitioned_at_ms.to_string()),
        extra: HashMap::new(),
    };
    match &state.new_status {
        baml_rt_provenance::metamodel::TaskStatusKind::InputRequired { prompt } => {
            status.message = Some(prompt_to_status_message(task_id, context_id, prompt));
        }
        baml_rt_provenance::metamodel::TaskStatusKind::Failed { reason } => {
            status.extra.insert(
                "error_reason".to_string(),
                serde_json::Value::String(reason.as_str().to_string()),
            );
        }
        _ => {}
    }
    status
}

#[async_trait]
impl TaskRepository for TaskSubgraphStore {
    async fn upsert(&self, task: Task) -> Result<Option<Task>> {
        // Graph-native: tasks come into existence through `TaskExists`
        // / `TaskExecutionStarted` provenance events. The wire-side
        // upsert is a no-op write because the wire `Task` has no
        // round-tripped fields anymore (`metadata` / `extra` were
        // intentionally dropped). Return the
        // input unchanged so the caller's pipeline observes the
        // post-upsert shape it expects.
        Ok(Some(task))
    }

    async fn ensure_task_exists(
        &self,
        task_id: &TaskId,
        context_id: Option<&ContextId>,
    ) -> Result<()> {
        // Idempotent. Emits `ProvEvent::TaskExists` when both
        // context and task ids are present so the graph carries a
        // first-class node and `SCOPED_TO` edge; degrades silently
        // (with a `tracing::warn!` breadcrumb) when context is absent
        // because graph-only writes require context.
        let Some(cid) = context_id else {
            tracing::warn!(
                task_id = %task_id.as_str(),
                "ensure_task_exists: no context_id — graph emission skipped"
            );
            return Ok(());
        };
        let event = ProvEvent::task_exists(cid.clone(), task_id.clone());
        self.writer
            .add_event(event)
            .await
            .map_err(Self::map_writer_err)?;
        Ok(())
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        // The wire `tasks.get` JSON-RPC carries only `{ id }`; the
        // owning context is reconstructed by walking the `SCOPED_TO`
        // edge from the Task node via
        // [`TaskGraphReader::resolve_by_task_id`].
        let task_id = TaskId::from_external(ExternalId::new(id));
        let scoped = match self.reader.resolve_by_task_id(&task_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.as_str(),
                    error = %err,
                    "TaskSubgraphStore::get resolve_by_task_id failed"
                );
                return None;
            }
        };
        match self.reader.hydrate(scoped, history_length).await {
            Ok(h) => Some(hydrated_to_wire_task(h, Vec::new())),
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.as_str(),
                    error = %err,
                    "TaskSubgraphStore::get hydrate failed"
                );
                None
            }
        }
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let scoped_result = match request.context_id.as_ref() {
            Some(ctx) => self.reader.list_scoped(ctx).await,
            None => self.reader.list_all().await,
        };
        let scoped = match scoped_result {
            Ok(v) => v,
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
        let hydrated: Vec<baml_rt_provenance::HydratedTask> = self
            .reader
            .hydrate_batch(&scoped, history_limit)
            .await
            .unwrap_or_default();

        let mut tasks: Vec<Task> = hydrated
            .into_iter()
            .map(|h| hydrated_to_wire_task(h, Vec::new()))
            .collect();

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
        // Resolve the owning context via the SCOPED_TO edge, emit a
        // TASK_STATE_CANCELED transition, then return the freshly
        // hydrated task. The canonical cancel flow inside
        // `apply_task_chunk` is unchanged; this entrypoint exists for
        // the wire-level `tasks.cancel` JSON-RPC handler that has only
        // `{ id }`.
        let task_id = TaskId::from_external(ExternalId::new(id));
        let scoped = match self.reader.resolve_by_task_id(&task_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.as_str(),
                    error = %err,
                    "TaskSubgraphStore::cancel resolve_by_task_id failed"
                );
                return None;
            }
        };
        let context_id = context_id_from_node_id(scoped.ctx().as_str());
        let event = ProvEvent::task_status_changed_typed(
            context_id.clone(),
            task_id.clone(),
            None,
            None,
            Some(baml_rt_provenance::metamodel::TaskStatusKind::Canceled),
        );
        if let Err(err) = self.writer.add_event(event).await {
            tracing::warn!(
                task_id = %task_id.as_str(),
                error = %err,
                "TaskSubgraphStore::cancel write failed"
            );
            return None;
        }
        match self.reader.hydrate(scoped, None).await {
            Ok(h) => Some(hydrated_to_wire_task(h, Vec::new())),
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.as_str(),
                    error = %err,
                    "TaskSubgraphStore::cancel hydrate failed"
                );
                None
            }
        }
    }

    async fn insert_message(&self, _message: &Message) -> Result<()> {
        // Graph-native: messages are persisted by the
        // `MessageReceived` / `MessageSent` ProvEvent emission already
        // performed by the surrounding boundary
        // (`SurrealRuntimeStore::emit_message_lifecycle_event`).
        // Calling this method is harmless; the relational
        // `a2a_message` mirror it previously wrote no longer exists.
        Ok(())
    }
}

#[async_trait]
impl TaskEventRecorder for TaskSubgraphStore {
    async fn record_status_update(
        &self,
        task_id: TaskId,
        context_id: ContextId,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let new_status_kind = wire_status_to_kind(&status)?;
        let new_state = new_status_kind.as_wire_str().to_string();

        // FSM gate: read the current head state from the graph and
        // apply the transition table.
        let scoped = self.scoped(&context_id, &task_id).await?;
        let current_state = if let Some(scoped) = &scoped {
            self.reader
                .latest_state(scoped.clone())
                .await
                .map_err(Self::map_writer_err)?
        } else {
            None
        };
        let allowed = match current_state
            .as_ref()
            .map(|state| state.new_status.as_wire_str())
        {
            None => new_state == S_SUBMITTED,
            Some(current) if is_terminal_state(current) => false,
            Some(current) => is_allowed_transition(current, &new_state),
        };
        if !allowed {
            return Ok(None);
        }

        // Emit the canonical ProvEvent. The normalizer's
        // `TaskStatusChanged` writes the immutable TaskState node,
        // links it to the previous state when known, and re-points the
        // latest-state head pointer in the same transaction.
        let event = ProvEvent::task_status_changed_typed(
            context_id.clone(),
            task_id.clone(),
            current_state.as_ref().map(|state| state.new_status.clone()),
            current_state
                .as_ref()
                .map(|state| state.activity_anchor.clone()),
            Some(new_status_kind.clone()),
        );
        let cursor = match &event {
            ProvEvent::Task(t) => {
                baml_rt_provenance::TaskReplayCursor::from_anchor(t.id.clone())
                    .map_err(|source| BamlRtError::InvalidArgument(source.to_string()))?
            }
            other => unreachable!("task_status_changed is always task-scoped: {other:?}"),
        };
        self.writer
            .add_event(event)
            .await
            .map_err(Self::map_writer_err)?;

        // Mirror onto the live broadcaster for SSE subscribers.
        let key = TaskStreamKey::new(context_id.clone(), task_id.clone());
        let writer = self.broadcaster.writer(key);
        let task_node = baml_rt_provenance::metamodel::TaskNodeId::for_task_id(&task_id);
        let frame = TaskUpdateFrame::StatusTransition {
            state: baml_rt_provenance::metamodel::A2ATaskStateProps::new(
                task_node,
                new_status_kind,
                current_state.as_ref().map(|state| state.new_status.clone()),
                status_timestamp_ms(&status),
                cursor.anchor().clone(),
            ),
            cursor,
        };
        let _ = writer.send(frame);
        if is_terminal_state(&new_state) {
            writer.retire_task();
        }

        Ok(Some(TaskUpdateEvent::Status(TaskStatusUpdateEvent {
            context_id: Some(context_id),
            task_id: Some(task_id),
            status: Some(status),
            metadata: None,
            extra: HashMap::new(),
        })))
    }

    async fn record_artifact_update(
        &self,
        task_id: TaskId,
        context_id: ContextId,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let artifact_type = artifact
            .metadata
            .as_ref()
            .and_then(|m| m.get("artifact_type"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let event = ProvEvent::task_artifact_generated(
            context_id.clone(),
            task_id.clone(),
            artifact.artifact_id.clone(),
            artifact_type.clone(),
        );
        let cursor = match &event {
            ProvEvent::Task(t) => {
                baml_rt_provenance::TaskReplayCursor::from_anchor(t.id.clone())
                    .map_err(|source| BamlRtError::InvalidArgument(source.to_string()))?
            }
            other => unreachable!("task_artifact_generated is always task-scoped: {other:?}"),
        };
        self.writer
            .add_event(event)
            .await
            .map_err(Self::map_writer_err)?;

        let key = TaskStreamKey::new(context_id.clone(), task_id.clone());
        let writer = self.broadcaster.writer(key);
        let frame = TaskUpdateFrame::ArtifactGenerated {
            artifact: ArtifactRef {
                task_id: task_id.clone(),
                artifact_id: artifact.artifact_id.clone(),
                artifact_type,
            },
            cursor,
        };
        let _ = writer.send(frame);

        Ok(Some(TaskUpdateEvent::Artifact(TaskArtifactUpdateEvent {
            context_id: Some(context_id),
            task_id: Some(task_id),
            last_chunk,
            append,
            artifact: Some(artifact),
            metadata: None,
            extra: HashMap::new(),
        })))
    }
}

fn status_timestamp_ms(status: &TaskStatus) -> u64 {
    status
        .timestamp
        .as_deref()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or_else(|| baml_rt_core::now_unix_ms(clock_events::A2A_STORE))
}

#[async_trait]
impl TaskChunkApplier for TaskSubgraphStore {
    async fn apply_task_chunk(&self, chunk: ValidatedTaskChunk) -> Result<Vec<TaskUpdateEvent>> {
        let task = chunk.task().cloned();
        let message = chunk.message().cloned();
        let status_update = chunk.status_update().cloned();
        let artifact_update = chunk.artifact_update().cloned();
        let mut out = Vec::new();
        let mut status_recorded_from_task: Option<(Option<TaskId>, Option<ContextId>, String)> =
            None;
        let mut input_required_transcript: Option<Message> = None;

        if let Some(mut t) = task {
            let status_snapshot = t.status.clone();
            let status = t.status.take();
            let context_id = t.context_id.clone();
            let task_id = t.id.clone();
            let artifacts = std::mem::take(&mut t.artifacts);
            // upsert is a no-op under the graph-only model.
            let _ = self.upsert(t).await?;
            if let Some(status) = status
                && let Some(tid) = &task_id
            {
                if let Some(ref cid) = context_id {
                    if let Some(ev) = self
                        .record_status_update(tid.clone(), cid.clone(), status.clone())
                        .await?
                    {
                        let state_str = wire_status_to_kind(&status)?.as_wire_str().to_string();
                        status_recorded_from_task =
                            Some((task_id.clone(), context_id.clone(), state_str));
                        out.push(ev);
                    }
                } else {
                    tracing::warn!(
                        task_id = %tid.as_str(),
                        "skipping task.status recording: chunk has no context_id"
                    );
                }
            }
            if message.is_none()
                && let (Some(tid), Some(cid), Some(st)) = (&task_id, &context_id, &status_snapshot)
            {
                input_required_transcript = input_required_transcript_message(tid, cid, st);
            }
            if let Some(tid) = task_id {
                for artifact in artifacts {
                    let Some(ref cid) = context_id else {
                        tracing::warn!(
                            task_id = %tid.as_str(),
                            "skipping artifact recording: chunk has no context_id"
                        );
                        continue;
                    };
                    if let Some(ev) = self
                        .record_artifact_update(
                            tid.clone(),
                            cid.clone(),
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
        if message.is_none()
            && input_required_transcript.is_none()
            && let Some(ref up) = status_update
            && let (Some(tid), Some(cid), Some(st)) =
                (&up.task_id, &up.context_id, up.status.as_ref())
        {
            input_required_transcript = input_required_transcript_message(tid, cid, st);
        }
        if let Some(msg) = message {
            self.insert_message(&msg).await?;
        } else if let Some(pm) = input_required_transcript {
            self.insert_message(&pm).await?;
        }
        if let Some(ref up) = status_update
            && let Some(status) = up.status.clone()
        {
            let state_str = wire_status_to_kind(&status)?.as_wire_str().to_string();
            let is_duplicate = status_recorded_from_task
                .as_ref()
                .is_some_and(|(tid, cid, s)| {
                    tid == &up.task_id && cid == &up.context_id && *s == state_str
                });
            if !is_duplicate && let Some(ref tid) = up.task_id {
                if let Some(ref cid) = up.context_id {
                    if let Some(ev) = self
                        .record_status_update(tid.clone(), cid.clone(), status)
                        .await?
                    {
                        out.push(ev);
                    }
                } else {
                    tracing::warn!(
                        task_id = %tid.as_str(),
                        "skipping status_update: chunk has no context_id"
                    );
                }
            }
        }
        if let Some(ref up) = artifact_update
            && let Some(ref tid) = up.task_id
        {
            if let Some(ref cid) = up.context_id {
                if let Some(ev) = self
                    .record_artifact_update(
                        tid.clone(),
                        cid.clone(),
                        up.artifact.clone().unwrap_or_default(),
                        up.append,
                        up.last_chunk,
                    )
                    .await?
                {
                    out.push(ev);
                }
            } else {
                tracing::warn!(
                    task_id = %tid.as_str(),
                    "skipping artifact_update: chunk has no context_id"
                );
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{AgentId, ExternalId, UuidId};
    use baml_rt_provenance::{SurrealStoreBuilder, events::ProvEvent};

    use super::*;

    fn make_agent_id() -> AgentId {
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000ab").unwrap())
    }

    fn wire_input_required_status(prompt: &str) -> TaskStatus {
        TaskStatus {
            state: Some(TaskState::String(S_INPUT_REQUIRED.to_string())),
            message: Some(Message {
                message_id: crate::a2a_types::A2aMessageId::incoming(ExternalId::new(
                    "msg-input-required",
                )),
                role: crate::a2a_types::MessageRole::Agent,
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
            timestamp: Some("1234".to_string()),
            extra: HashMap::new(),
        }
    }

    fn wire_failed_status(reason: &str) -> TaskStatus {
        let mut extra = HashMap::new();
        extra.insert(
            "error_reason".to_string(),
            serde_json::Value::String(reason.to_string()),
        );
        TaskStatus {
            state: Some(TaskState::String(S_FAILED.to_string())),
            message: None,
            timestamp: Some("5678".to_string()),
            extra,
        }
    }

    async fn build_task_store() -> (
        Arc<baml_rt_provenance::SurrealProvenanceStore>,
        TaskSubgraphStore,
        ContextId,
        TaskId,
    ) {
        let provenance = SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build isolated provenance store");
        let reader: Arc<dyn TaskGraphReader> = provenance.clone();
        let writer: Arc<dyn ProvenanceWriter> = provenance.clone();
        let task_store = TaskSubgraphStore::new(reader, writer);
        let context_id = ContextId::new(7_001, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-status-totality"));
        provenance
            .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
            .await
            .expect("task_exists");
        provenance
            .add_event(ProvEvent::task_execution_started(
                context_id.clone(),
                task_id.clone(),
                make_agent_id(),
            ))
            .await
            .expect("task_execution_started");
        (provenance, task_store, context_id, task_id)
    }

    #[tokio::test]
    async fn get_and_list_preserve_input_required_prompt() {
        let (_provenance, task_store, context_id, task_id) = build_task_store().await;

        task_store
            .record_status_update(
                task_id.clone(),
                context_id.clone(),
                TaskStatus {
                    state: Some(TaskState::String(S_SUBMITTED.to_string())),
                    message: None,
                    timestamp: Some("1000".to_string()),
                    extra: HashMap::new(),
                },
            )
            .await
            .expect("submitted status");
        task_store
            .record_status_update(
                task_id.clone(),
                context_id.clone(),
                wire_input_required_status("Please confirm the deploy window"),
            )
            .await
            .expect("input required status");

        let task = task_store
            .get(task_id.as_str(), None)
            .await
            .expect("task snapshot");
        let prompt = task
            .status
            .as_ref()
            .and_then(|status| status.message.as_ref())
            .and_then(|message| message.parts.first())
            .and_then(|part| part.text.as_deref());
        assert_eq!(
            prompt,
            Some("Please confirm the deploy window"),
            "tasks.get must preserve the input-required prompt",
        );

        let listed = task_store
            .list(&ListTasksRequest {
                context_id: Some(context_id),
                history_length: None,
                include_artifacts: Some(true),
                page_size: None,
                page_token: None,
                status: None,
                status_timestamp_after: None,
                tenant: None,
                extra: HashMap::new(),
            })
            .await;
        let listed_prompt = listed
            .tasks
            .first()
            .and_then(|task| task.status.as_ref())
            .and_then(|status| status.message.as_ref())
            .and_then(|message| message.parts.first())
            .and_then(|part| part.text.as_deref());
        assert_eq!(
            listed_prompt,
            Some("Please confirm the deploy window"),
            "tasks.list must preserve the input-required prompt",
        );
    }

    #[tokio::test]
    async fn get_preserves_failed_reason() {
        let (_provenance, task_store, context_id, task_id) = build_task_store().await;

        task_store
            .record_status_update(
                task_id.clone(),
                context_id.clone(),
                TaskStatus {
                    state: Some(TaskState::String(S_SUBMITTED.to_string())),
                    message: None,
                    timestamp: Some("1000".to_string()),
                    extra: HashMap::new(),
                },
            )
            .await
            .expect("submitted status");
        task_store
            .record_status_update(
                task_id.clone(),
                context_id,
                wire_failed_status("quota exhausted"),
            )
            .await
            .expect("failed status");

        let task = task_store
            .get(task_id.as_str(), None)
            .await
            .expect("failed task snapshot");
        let reason = task
            .status
            .as_ref()
            .and_then(|status| status.extra.get("error_reason"))
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            reason,
            Some("quota exhausted"),
            "tasks.get must preserve the original failed reason",
        );
    }

    #[tokio::test]
    async fn malformed_payload_bearing_statuses_are_rejected() {
        let (_provenance, task_store, context_id, task_id) = build_task_store().await;

        let err = task_store
            .record_status_update(
                task_id.clone(),
                context_id.clone(),
                TaskStatus {
                    state: Some(TaskState::String(S_INPUT_REQUIRED.to_string())),
                    message: None,
                    timestamp: Some("1000".to_string()),
                    extra: HashMap::new(),
                },
            )
            .await
            .expect_err("missing input-required prompt must be rejected");
        assert!(
            err.to_string()
                .contains("TASK_STATE_INPUT_REQUIRED requires"),
            "strict prompt error should mention the missing prompt payload, got: {err}",
        );

        let err = task_store
            .record_status_update(
                task_id,
                context_id,
                TaskStatus {
                    state: Some(TaskState::String(S_FAILED.to_string())),
                    message: None,
                    timestamp: Some("1001".to_string()),
                    extra: HashMap::new(),
                },
            )
            .await
            .expect_err("missing failed reason must be rejected");
        assert!(
            err.to_string().contains("TASK_STATE_FAILED requires"),
            "strict failure error should mention the missing reason payload, got: {err}",
        );
    }
}
