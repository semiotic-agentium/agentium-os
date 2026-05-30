// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::{
    BamlRtError, Citation, Result, clock_events,
    ids::{AgentId, ContextId, TaskId},
};
use baml_rt_observability::metrics;
use baml_rt_provenance::{
    ProvEvent, ProvenanceWriter,
    metamodel::{NonEmptyString, TaskStatusKind},
};
use serde_json::Value;

use crate::a2a_types::{
    Artifact, ListTasksRequest, ListTasksResponse, Message, MessageRole, Part, ROLE_USER, Task,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent, ValidatedTaskChunk,
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
    /// Record a status transition for an A2A task.
    ///
    /// `context_id` is required: the provenance graph is the sole source
    /// of truth for tasks/messages/status, and a `context_id`-less
    /// status update would silently drop the
    /// `ProvEvent::TaskStatusChanged` emission. Callers that only have
    /// `Option<ContextId>` must hard-fail or skip-with-logged-degradation
    /// at the call site, never pass a synthesised default.
    async fn record_status_update(
        &self,
        task_id: TaskId,
        context_id: ContextId,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>>;
    /// Record an artifact update for an A2A task.
    ///
    /// `context_id` is required: the provenance graph is the sole source of
    /// truth for tasks/messages/artifacts, and a `context_id`-less
    /// artifact update would silently drop the `ProvEvent::TaskArtifactGenerated`
    /// emission. Callers that only have `Option<ContextId>` must hard-fail
    /// or skip-with-logged-degradation at the call site, never pass a
    /// synthesised default.
    async fn record_artifact_update(
        &self,
        task_id: TaskId,
        context_id: ContextId,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>>;
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
pub trait TaskStoreBackend: TaskRepository + TaskEventRecorder + TaskChunkApplier {}

impl<T> TaskStoreBackend for T where T: TaskRepository + TaskEventRecorder + TaskChunkApplier {}

pub(crate) fn now_millis() -> u64 {
    baml_rt_core::now_unix_ms(clock_events::A2A_STORE)
}

pub(crate) fn require_context_id(
    context_id: Option<ContextId>,
    operation: &str,
) -> Result<ContextId> {
    context_id.ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "context_id is required for {operation}; refusing implicit generation",
        ))
    })
}

pub(crate) fn ensure_agent_id_in_metadata(
    metadata: &mut Option<HashMap<String, Value>>,
    agent_id: &AgentId,
) {
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

pub(crate) fn record_task_store_metrics(op: &str, outcome: &str, start: Instant) {
    metrics::record_task_store_operation(op, outcome, start.elapsed());
}

pub(crate) fn inject_agent_id_into_chunk(
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

// Bug A.3 chunk-message resolution now lives on `ValidatedTaskChunk` itself
// (`resolved_assistant_message()` + `ResolvedAssistantMessage::scoped_owned()`).
// Centralising the precedence and scope backfill next to the validator keeps
// the wire invariant compile-adjacent and lets the hot path (top-level
// `StreamResponse.message`) stay zero-clone until provenance actually needs the
// owned `Message`.

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

/// Extract validated `Citation`s from `metadata.citations` (the wire-side typed
/// ref-table strings: `#N`, `@N`, …). Only present on assistant emissions; user
/// turns return an empty vec. Parse failures are skipped with a warn-level log
/// so a malformed string doesn't sink the lifecycle write.
pub(crate) fn extract_message_citations(message: &Message) -> Vec<Citation> {
    message
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
        .unwrap_or_default()
}

/// Inputs required to build a `ProvEvent::message_{received,sent}_{task,global}`.
///
/// Replaces the near-identical `match (role.as_str(), task_id.clone())`
/// blocks in the graph-backed runtime store with one builder so the constructor
/// matrix lives in exactly one place.
///
/// Borrows everything; the only allocations are inside `ProvEvent::message_*`
/// constructors themselves (which clone what they need).
pub(crate) struct MessageLifecycleEventInput<'a> {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub message_id: baml_rt_core::ids::MessageId,
    pub role: String,
    pub content: Vec<String>,
    pub metadata: Option<HashMap<String, String>>,
    pub citations: Vec<Citation>,
    pub agent_id: &'a AgentId,
    pub timestamp_ms: u64,
}

impl<'a> MessageLifecycleEventInput<'a> {
    /// Resolve the (`role`, `task_id`) cross-product into the right `ProvEvent`
    /// constructor. `citations` are only attached to `MessageSent` variants
    /// (`MessageReceived` constructors do not accept them).
    pub fn into_prov_event(self) -> ProvEvent {
        let MessageLifecycleEventInput {
            context_id,
            task_id,
            message_id,
            role,
            content,
            metadata,
            citations,
            agent_id,
            timestamp_ms,
        } = self;
        let agent_id = agent_id.clone();
        let is_user = role == ROLE_USER;
        match (is_user, task_id) {
            (true, Some(task_id)) => ProvEvent::message_received_task(
                context_id,
                task_id,
                message_id,
                role,
                content,
                metadata,
                agent_id,
                timestamp_ms,
            ),
            (true, None) => ProvEvent::message_received_global(
                context_id,
                message_id,
                role,
                content,
                metadata,
                agent_id,
                timestamp_ms,
            ),
            (false, Some(task_id)) => ProvEvent::message_sent_task(
                context_id,
                task_id,
                message_id,
                role,
                content,
                metadata,
                agent_id,
                timestamp_ms,
                citations,
            ),
            (false, None) => ProvEvent::message_sent_global(
                context_id,
                message_id,
                role,
                content,
                metadata,
                agent_id,
                timestamp_ms,
                citations,
            ),
        }
    }

    /// Build a complete input from a wire `Message` and an explicit scope. The
    /// caller supplies the agent_id (always known at the writer site) and a
    /// timestamp (caller picks `clock_events::A2A_STORE` or `A2A_TRANSPORT`
    /// per their own context).
    ///
    /// `validated_message_content` errors propagate to the caller — content
    /// extraction failure is the only way this constructor can fail.
    pub fn from_message(
        message: &Message,
        context_id: ContextId,
        task_id: Option<TaskId>,
        agent_id: &'a AgentId,
        timestamp_ms: u64,
        operation: &str,
    ) -> Result<Self> {
        let role = message_role_string(&message.role);
        let content = validated_message_content(message, operation)?;
        let metadata = message.metadata.as_ref().map(metadata_string_map);
        let citations = if role == ROLE_USER {
            // Inbound user turns never carry typed citations; skip the
            // metadata.citations array regardless of what the wire put there.
            Vec::new()
        } else {
            extract_message_citations(message)
        };
        Ok(MessageLifecycleEventInput {
            context_id,
            task_id,
            message_id: message.message_id.as_message_id().clone(),
            role,
            content,
            metadata,
            citations,
            agent_id,
            timestamp_ms,
        })
    }
}

pub(crate) fn status_to_string(status: &TaskStatus) -> Option<String> {
    status.state.as_ref().map(|state| match state {
        TaskState::String(value) => value.clone(),
        TaskState::Integer(value) => value.to_string(),
    })
}

pub(crate) fn transcript_text_from_wire_status_message(msg: &Message) -> Option<String> {
    let mut lines = Vec::new();
    for part in &msg.parts {
        if let Some(text) = part.text.as_deref().map(str::trim)
            && !text.is_empty()
        {
            lines.push(text.to_string());
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// Success witness for wire `TASK_STATE_FAILED`: a validated, non-empty [`NonEmptyString`] reason.
/// JSON remains [`TaskStatus`]; callers use [`Self::try_parse`] so `TaskStatusKind::Failed` is only
/// constructed when the wire carried an explicit failure payload (not a host default).
#[derive(Debug, Clone)]
pub(crate) struct WireFailedTaskStatus {
    pub(crate) reason: NonEmptyString,
}

impl WireFailedTaskStatus {
    pub(crate) fn try_parse(status: &TaskStatus) -> Result<Self> {
        fn reason_from_extra(extra: &HashMap<String, Value>) -> Option<&str> {
            for key in ["error_reason", "errorReason"] {
                if let Some(s) = extra.get(key).and_then(Value::as_str) {
                    let t = s.trim();
                    if !t.is_empty() {
                        return Some(t);
                    }
                }
            }
            None
        }

        let from_extra = reason_from_extra(&status.extra);
        let from_message = status
            .message
            .as_ref()
            .and_then(transcript_text_from_wire_status_message)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let reason_str = from_extra
            .map(str::to_string)
            .or(from_message)
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "TASK_STATE_FAILED requires a non-empty failure reason; set `errorReason` (or `error_reason`) on the status object and/or non-empty `message.parts[].text`"
                        .to_string(),
                )
            })?;

        let reason = NonEmptyString::new(reason_str).map_err(|_| {
            BamlRtError::InvalidArgument(
                "TASK_STATE_FAILED failure reason was empty after normalization".to_string(),
            )
        })?;

        Ok(Self { reason })
    }
}

pub(crate) fn wire_status_to_kind(status: &TaskStatus) -> Result<TaskStatusKind> {
    let raw = status_to_string(status).ok_or_else(|| {
        BamlRtError::InvalidArgument(
            "task status update is missing `status.state`; payload-bearing statuses must be explicit"
                .to_string(),
        )
    })?;
    match raw.as_str() {
        S_INPUT_REQUIRED => {
            let prompt = status
                .message
                .as_ref()
                .and_then(transcript_text_from_wire_status_message)
                .ok_or_else(|| {
                    BamlRtError::InvalidArgument(
                        "TASK_STATE_INPUT_REQUIRED requires non-empty `status.message.parts[].text`"
                            .to_string(),
                    )
                })?;
            Ok(TaskStatusKind::InputRequired { prompt })
        }
        S_FAILED => {
            let validated = WireFailedTaskStatus::try_parse(status)?;
            Ok(TaskStatusKind::Failed {
                reason: validated.reason,
            })
        }
        _ => parse_tag_only_task_status_kind(raw.as_str()).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!("unsupported task status state {raw:?}"))
        }),
    }
}

fn parse_tag_only_task_status_kind(raw: &str) -> Option<TaskStatusKind> {
    match raw {
        S_SUBMITTED | "submitted" => Some(TaskStatusKind::Submitted),
        S_WORKING | "working" => Some(TaskStatusKind::Working),
        S_AUTH_REQUIRED | "auth-required" | "auth_required" => Some(TaskStatusKind::AuthRequired),
        S_COMPLETED | "completed" => Some(TaskStatusKind::Completed),
        S_CANCELED | "canceled" | "cancelled" => Some(TaskStatusKind::Canceled),
        S_REJECTED | "rejected" => Some(TaskStatusKind::Rejected),
        _ => None,
    }
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

#[cfg(test)]
mod strip_wire_task_id_tests {
    use std::collections::HashMap;

    use serde_json::Value;

    use super::{
        Task, TaskState, TaskStatus, should_strip_wire_task_id_for_message_send_stream,
        wire_status_to_kind,
    };

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

    #[test]
    fn wire_status_failed_accepts_message_text_as_reason() {
        use baml_rt_core::ids::ExternalId;
        use baml_rt_provenance::metamodel::TaskStatusKind;

        use crate::a2a_types::{A2aMessageId, Message, MessageRole, Part, TaskState, TaskStatus};

        let status = TaskStatus {
            state: Some(TaskState::String("TASK_STATE_FAILED".to_string())),
            message: Some(Message {
                message_id: A2aMessageId::incoming(ExternalId::new("m")),
                role: MessageRole::Agent,
                parts: vec![Part {
                    text: Some("boom from agent".to_string()),
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
        };
        match wire_status_to_kind(&status).expect("parses") {
            TaskStatusKind::Failed { reason } => {
                assert_eq!(reason.as_str(), "boom from agent");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn wire_status_failed_without_reason_or_message_is_rejected() {
        use crate::a2a_types::{TaskState, TaskStatus};

        let status = TaskStatus {
            state: Some(TaskState::String("TASK_STATE_FAILED".to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };
        let err =
            wire_status_to_kind(&status).expect_err("missing failure payload must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("TASK_STATE_FAILED") && msg.contains("non-empty"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_status_failed_accepts_error_reason_camel_case_in_extra() {
        use baml_rt_provenance::metamodel::TaskStatusKind;

        use crate::a2a_types::{TaskState, TaskStatus};

        let mut extra = HashMap::new();
        extra.insert(
            "errorReason".to_string(),
            Value::String("quota".to_string()),
        );
        let status = TaskStatus {
            state: Some(TaskState::String("TASK_STATE_FAILED".to_string())),
            message: None,
            timestamp: None,
            extra,
        };
        match wire_status_to_kind(&status).expect("parses") {
            TaskStatusKind::Failed { reason } => assert_eq!(reason.as_str(), "quota"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

/// Bug A.3 regression: assistant emissions arriving in `statusUpdate.status.message`
/// (or in `task.status.message`) MUST land in provenance as a Message lifecycle event,
/// not be silently dropped. Without these tests the fix is a no-op the next time
/// someone refactors `apply_task_chunk`.
#[cfg(test)]
mod nested_assistant_message_persistence_tests {
    use std::collections::HashMap;

    use baml_rt::BamlRuntimeManager;
    use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
    use baml_rt_provenance::{ProvenanceContextReader, SurrealStoreBuilder, surreal_store};
    use uuid::Uuid;

    use super::*;
    use crate::a2a_types::{
        A2aMessageId, MessageRole, Part, StreamResponse, TaskState, TaskStatus,
        TaskStatusUpdateEvent,
    };

    fn make_assistant_message(id: &str, ctx: &ContextId, task: &TaskId, text: &str) -> Message {
        Message {
            message_id: A2aMessageId::incoming(ExternalId::new(id)),
            role: MessageRole::Agent,
            parts: vec![Part {
                text: Some(text.to_string()),
                ..Default::default()
            }],
            context_id: Some(ctx.clone()),
            task_id: Some(task.clone()),
            reference_task_ids: Vec::new(),
            extensions: Vec::new(),
            metadata: None,
            extra: HashMap::new(),
        }
    }

    async fn build_store() -> (
        std::sync::Arc<surreal_store::SurrealProvenanceStore>,
        std::sync::Arc<dyn TaskStoreBackend>,
        ContextId,
        TaskId,
    ) {
        let prov = SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("isolated provenance store");
        let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
        let agent = crate::A2aAgent::builder()
            .with_agent_id(agent_id)
            .with_runtime_manager(
                BamlRuntimeManager::builder()
                    .build()
                    .expect("runtime manager"),
            )
            .with_effect_emitter(std::sync::Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_surreal_store(prov.clone())
            .build()
            .await
            .expect("graph-backed agent");
        let task_store = agent.task_store();
        let ctx = ContextId::new(20240512, 1);
        let task = TaskId::for_live_stream(&ctx, &MessageId::from("probe"));
        (prov, task_store, ctx, task)
    }

    fn make_task_envelope(ctx: &ContextId, task: &TaskId) -> crate::a2a_types::Task {
        crate::a2a_types::Task {
            id: Some(task.clone()),
            context_id: Some(ctx.clone()),
            artifacts: Vec::new(),
            history: Vec::new(),
            status: None,
            metadata: None,
            extra: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn assistant_message_in_status_update_is_persisted() {
        let (prov, task_store, ctx, task) = build_store().await;
        let assistant_text = "What specific task do you need help with?";
        let assistant_message_id = "js-msg-await-1";

        let chunk = ValidatedTaskChunk::try_from(StreamResponse {
            message: None,
            task: Some(make_task_envelope(&ctx, &task)),
            status_update: Some(TaskStatusUpdateEvent {
                context_id: Some(ctx.clone()),
                task_id: Some(task.clone()),
                status: Some(TaskStatus {
                    state: Some(TaskState::String("TASK_STATE_INPUT_REQUIRED".to_string())),
                    message: Some(make_assistant_message(
                        assistant_message_id,
                        &ctx,
                        &task,
                        assistant_text,
                    )),
                    timestamp: None,
                    extra: HashMap::new(),
                }),
                metadata: None,
                extra: HashMap::new(),
            }),
            artifact_update: None,
            extra: HashMap::new(),
        })
        .expect("validate awaitInput-shaped chunk");

        task_store
            .apply_task_chunk(chunk)
            .await
            .expect("apply_task_chunk for awaitInput-shaped status update");

        let messages = prov
            .context_messages(&ctx, None)
            .await
            .expect("read context messages");
        let assistant_rows: Vec<_> = messages
            .iter()
            .filter(|m| m.message_id.as_str() == assistant_message_id)
            .collect();
        assert_eq!(
            assistant_rows.len(),
            1,
            "exactly one assistant Message should be persisted from status_update; rows={messages:?}"
        );
        let assistant = assistant_rows[0];
        assert_eq!(assistant.role, "ROLE_AGENT");
        assert!(
            assistant.content.iter().any(|c| c.contains(assistant_text)),
            "assistant content missing expected text {assistant_text:?}; got {:?}",
            assistant.content
        );
    }

    #[tokio::test]
    async fn assistant_message_in_task_status_is_persisted() {
        let (prov, task_store, ctx, task) = build_store().await;
        let assistant_text = "Reply to continue the conversation.";
        let assistant_message_id = "js-msg-task-status-1";

        let chunk = ValidatedTaskChunk::try_from(StreamResponse {
            message: None,
            task: Some(crate::a2a_types::Task {
                id: Some(task.clone()),
                context_id: Some(ctx.clone()),
                artifacts: Vec::new(),
                history: Vec::new(),
                status: Some(TaskStatus {
                    state: Some(TaskState::String("TASK_STATE_INPUT_REQUIRED".to_string())),
                    message: Some(make_assistant_message(
                        assistant_message_id,
                        &ctx,
                        &task,
                        assistant_text,
                    )),
                    timestamp: None,
                    extra: HashMap::new(),
                }),
                metadata: None,
                extra: HashMap::new(),
            }),
            status_update: None,
            artifact_update: None,
            extra: HashMap::new(),
        })
        .expect("validate task-status-shaped chunk");

        task_store
            .apply_task_chunk(chunk)
            .await
            .expect("apply_task_chunk for task.status.message-shaped chunk");

        let messages = prov
            .context_messages(&ctx, None)
            .await
            .expect("read context messages");
        assert!(
            messages
                .iter()
                .any(|m| m.message_id.as_str() == assistant_message_id),
            "assistant message lifted from task.status.message must be persisted; rows={messages:?}"
        );
    }

    /// Top-level `StreamResponse.message` must still take precedence over the nested
    /// fallbacks when both are populated — this preserves the legacy behaviour where
    /// task chunks carrying both shapes don't double-write.
    #[tokio::test]
    async fn top_level_message_takes_precedence_over_nested_fallback() {
        let (prov, task_store, ctx, task) = build_store().await;
        let top_text = "top-level wins";
        let nested_text = "nested loses";
        let chunk = ValidatedTaskChunk::try_from(StreamResponse {
            message: Some(make_assistant_message("js-msg-top", &ctx, &task, top_text)),
            task: Some(make_task_envelope(&ctx, &task)),
            status_update: Some(TaskStatusUpdateEvent {
                context_id: Some(ctx.clone()),
                task_id: Some(task.clone()),
                status: Some(TaskStatus {
                    state: Some(TaskState::String("TASK_STATE_WORKING".to_string())),
                    message: Some(make_assistant_message(
                        "js-msg-nested",
                        &ctx,
                        &task,
                        nested_text,
                    )),
                    timestamp: None,
                    extra: HashMap::new(),
                }),
                metadata: None,
                extra: HashMap::new(),
            }),
            artifact_update: None,
            extra: HashMap::new(),
        })
        .expect("validate dual-shape chunk");

        task_store
            .apply_task_chunk(chunk)
            .await
            .expect("apply chunk");

        let messages = prov
            .context_messages(&ctx, None)
            .await
            .expect("context messages");
        assert!(
            messages
                .iter()
                .any(|m| m.message_id.as_str() == "js-msg-top"),
            "expected top-level message to be persisted"
        );
        assert!(
            messages
                .iter()
                .all(|m| m.message_id.as_str() != "js-msg-nested"),
            "nested fallback must not double-write when top-level is present; rows={messages:?}"
        );
    }
}
