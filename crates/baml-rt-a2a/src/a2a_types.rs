use std::collections::HashMap;

use baml_rt_core::{
    BamlRtError,
    ids::{ArtifactId, ContextId, DerivedId, ExternalId, MessageId, TaskId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MESSAGE_KIND: &str = "message";
pub const ROLE_USER: &str = "ROLE_USER";
pub const ROLE_AGENT: &str = "ROLE_AGENT";
pub const TASK_STATE_CANCELED: &str = "TASK_STATE_CANCELED";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JSONRPCId {
    String(String),
    Integer(i64),
    #[default]
    Null,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONRPCRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub id: Option<JSONRPCId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONRPCSuccessResponse {
    pub jsonrpc: String,
    pub result: Value,
    pub id: Option<JSONRPCId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JSONRPCError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JSONRPCErrorResponse {
    pub jsonrpc: String,
    pub error: JSONRPCError,
    pub id: Option<JSONRPCId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    #[serde(rename = "ROLE_USER", alias = "user", alias = "USER")]
    User,
    #[serde(
        rename = "ROLE_AGENT",
        alias = "agent",
        alias = "AGENT",
        alias = "assistant",
        alias = "ASSISTANT",
        alias = "ROLE_ASSISTANT"
    )]
    Agent,
}

impl MessageRole {
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::User => ROLE_USER,
            Self::Agent => ROLE_AGENT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskState {
    String(String),
    Integer(i64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NumberOrString {
    Number(i64),
    String(String),
}

impl NumberOrString {
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            NumberOrString::Number(value) if *value >= 0 => Some(*value as usize),
            NumberOrString::String(value) => value.parse::<usize>().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageIdKind {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2aMessageId {
    id: MessageId,
    kind: MessageIdKind,
}

impl A2aMessageId {
    pub fn incoming(id: ExternalId) -> Self {
        Self {
            id: MessageId::from_external(id),
            kind: MessageIdKind::Incoming,
        }
    }

    pub fn outgoing(id: DerivedId) -> Self {
        Self {
            id: MessageId::from_derived(id),
            kind: MessageIdKind::Outgoing,
        }
    }

    pub fn as_message_id(&self) -> &MessageId {
        &self.id
    }

    pub fn into_message_id(self) -> MessageId {
        self.id
    }

    pub fn kind(&self) -> MessageIdKind {
        self.kind.clone()
    }
}

impl Serialize for A2aMessageId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.id.as_str())
    }
}

impl<'de> Deserialize<'de> for A2aMessageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(A2aMessageId::incoming(ExternalId::new(raw)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub message_id: A2aMessageId,
    pub role: MessageRole,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default)]
    pub reference_task_ids: Vec<TaskId>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<ArtifactId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub history: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageConfiguration {
    #[serde(default)]
    pub accepted_output_modes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<NumberOrString>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<SendMessageConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskRequest {
    pub id: TaskId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_artifacts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<NumberOrString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_timestamp_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksResponse {
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u64>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeToTaskRequest {
    pub id: TaskId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl TaskStatusUpdateEvent {
    /// Builds a status-update event from a task's current status, if present.
    pub fn from_task_current_status(task: &Task) -> Option<Self> {
        let status = task.status.clone()?;
        Some(Self {
            context_id: task.context_id.clone(),
            task_id: task.id.clone(),
            status: Some(status),
            metadata: None,
            extra: HashMap::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_chunk: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Artifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Stream chunk variant for tasks.subscribe stream responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamChunk {
    Task {
        #[serde(flatten)]
        task: Task,
        #[serde(flatten)]
        extra: HashMap<String, Value>,
    },
    StatusUpdate {
        #[serde(flatten)]
        status_update: TaskStatusUpdateEvent,
        #[serde(flatten)]
        extra: HashMap<String, Value>,
    },
    ArtifactUpdate {
        #[serde(flatten)]
        artifact_update: TaskArtifactUpdateEvent,
        #[serde(flatten)]
        extra: HashMap<String, Value>,
    },
}

impl StreamChunk {
    pub fn task(task: Task) -> Self {
        StreamChunk::Task {
            task,
            extra: HashMap::new(),
        }
    }

    pub fn status_update(status_update: TaskStatusUpdateEvent) -> Self {
        StreamChunk::StatusUpdate {
            status_update,
            extra: HashMap::new(),
        }
    }

    pub fn artifact_update(artifact_update: TaskArtifactUpdateEvent) -> Self {
        StreamChunk::ArtifactUpdate {
            artifact_update,
            extra: HashMap::new(),
        }
    }
}

/// Typed view of a stream chunk (wire format). Parse once at the boundary; use accessors for routing and decisions.
/// IDs are sourced from [baml_rt_core::ids]. Holds the raw value for forwarding.
#[derive(Debug, Clone)]
pub struct StreamChunkView {
    /// Parsed task id from `task.id` or `statusUpdate.taskId`.
    task_id: Option<TaskId>,
    /// Parsed state from `task.status.state` or `statusUpdate.status.state`.
    task_state: Option<String>,
    has_status_update: bool,
    has_artifact_update: bool,
    has_task: bool,
    /// Original chunk for forwarding and store_result.
    pub raw: Value,
}

impl StreamChunkView {
    const MAX_TASK_ID_PARSE_DEPTH: usize = 4;

    /// Build a typed view from the wire chunk. Parses task_id and task_state once; use accessors thereafter.
    pub fn new(value: Value) -> Self {
        let task_id = Self::parse_task_id(&value);
        let task_state = Self::parse_task_state(&value);
        let has_status_update = value
            .get("statusUpdate")
            .or_else(|| value.get("status_update"))
            .is_some();
        let has_artifact_update = value
            .get("artifactUpdate")
            .or_else(|| value.get("artifact_update"))
            .is_some();
        let has_task = value.get("task").is_some();
        Self {
            task_id,
            task_state,
            has_status_update,
            has_artifact_update,
            has_task,
            raw: value,
        }
    }

    fn value_or_parsed_json(value: &Value) -> Option<Value> {
        match value {
            Value::Object(_) => Some(value.clone()),
            Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
            _ => None,
        }
    }

    fn parse_task_id(v: &Value) -> Option<TaskId> {
        let id_str = Self::extract_task_id_str(v)?;
        Some(TaskId::from_external(ExternalId::new(id_str)))
    }

    fn extract_task_id_str(v: &Value) -> Option<String> {
        Self::extract_task_id_str_with_depth(v, 0)
    }

    fn extract_task_id_str_with_depth(v: &Value, depth: usize) -> Option<String> {
        if depth > Self::MAX_TASK_ID_PARSE_DEPTH {
            return None;
        }

        if let Some(raw_json) = v.as_str() {
            let parsed = serde_json::from_str::<Value>(raw_json).ok()?;
            return Self::extract_task_id_str_with_depth(&parsed, depth + 1);
        }

        let from_task = || {
            v.get("task")
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        };
        let from_direct_id = || v.get("id").and_then(Value::as_str).map(ToOwned::to_owned);
        let from_stringified_task = || {
            let raw = v.get("task").and_then(Value::as_str)?;
            let parsed = serde_json::from_str::<Value>(raw).ok()?;
            Self::extract_task_id_str_with_depth(&parsed, depth + 1)
        };
        let from_status_update = || {
            let status_update = v.get("statusUpdate").or_else(|| v.get("status_update"))?;
            status_update
                .get("taskId")
                .or_else(|| {
                    status_update
                        .get("statusUpdate")
                        .and_then(|nested| nested.get("taskId"))
                })
                .or_else(|| {
                    status_update
                        .get("status_update")
                        .and_then(|nested| nested.get("taskId"))
                })
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        };
        let from_stringified_status_update = || {
            let raw = v
                .get("statusUpdate")
                .or_else(|| v.get("status_update"))
                .and_then(Value::as_str)?;
            let parsed = serde_json::from_str::<Value>(raw).ok()?;
            Self::extract_task_id_str_with_depth(&parsed, depth + 1)
        };
        let from_message = || {
            v.get("message")
                .and_then(|message| message.get("taskId"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        };
        let from_top_level = || {
            v.get("taskId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        };
        let from_chunks_array = || {
            let chunks = v.get("chunks")?.as_array()?;
            for chunk in chunks {
                if let Some(task_id) = Self::extract_task_id_str_with_depth(chunk, depth + 1) {
                    return Some(task_id);
                }
            }
            None
        };
        let from_chunk = || {
            let chunk = v.get("chunk")?;
            Self::extract_task_id_str_with_depth(chunk, depth + 1)
        };

        from_task()
            .or_else(from_direct_id)
            .or_else(from_stringified_task)
            .or_else(from_status_update)
            .or_else(from_stringified_status_update)
            .or_else(from_message)
            .or_else(from_top_level)
            .or_else(from_chunks_array)
            .or_else(from_chunk)
    }

    fn parse_task_state(v: &Value) -> Option<String> {
        let state_from = |val: &Value| {
            val.get("status")
                .and_then(|s| s.get("state"))
                .and_then(|s| s.as_str())
                .map(String::from)
        };
        v.get("task")
            .and_then(Self::value_or_parsed_json)
            .as_ref()
            .and_then(state_from)
            .or_else(|| {
                let su = v
                    .get("statusUpdate")
                    .or_else(|| v.get("status_update"))
                    .and_then(Self::value_or_parsed_json)?;
                let ev = if su.get("status").is_some() {
                    su
                } else {
                    su.get("statusUpdate")
                        .or_else(|| su.get("status_update"))?
                        .clone()
                };
                state_from(&ev)
            })
    }

    pub fn is_null(&self) -> bool {
        match &self.raw {
            Value::Null => true,
            Value::Object(map) => map.is_empty(),
            _ => false,
        }
    }

    /// Wrap a typed stream payload for the same accessors as [`Self::new`] (serializes once).
    pub fn from_stream_response(sr: &StreamResponse) -> Self {
        Self::new(serde_json::to_value(sr).unwrap_or(Value::Null))
    }

    pub fn task_id(&self) -> Option<&TaskId> {
        self.task_id.as_ref()
    }

    pub fn task_state(&self) -> Option<&str> {
        self.task_state.as_deref()
    }

    /// True if chunk carries a non-COMPLETED terminal state (FAILED, REJECTED, CANCELED).
    pub fn is_non_completed_terminal(&self) -> bool {
        self.task_state().is_some_and(|s| {
            matches!(
                s,
                "TASK_STATE_FAILED" | "TASK_STATE_REJECTED" | "TASK_STATE_CANCELED"
            )
        })
    }

    pub fn has_status_update(&self) -> bool {
        self.has_status_update
    }

    pub fn has_artifact_update(&self) -> bool {
        self.has_artifact_update
    }

    pub fn has_task(&self) -> bool {
        self.has_task
    }

    /// True if this chunk should be considered for store_result (has task-related or status/artifact payload).
    pub fn has_storable_payload(&self) -> bool {
        self.has_status_update || self.has_artifact_update || self.has_task
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StreamResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_update: Option<TaskStatusUpdateEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_update: Option<TaskArtifactUpdateEvent>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Stream chunk accepted by [`crate::a2a_store::TaskChunkApplier`]: invariant that
/// `status_update` / `artifact_update` require `task` in the same payload is enforced here,
/// not left to each store implementation.
#[derive(Debug, Clone)]
pub struct ValidatedTaskChunk(StreamResponse);

impl ValidatedTaskChunk {
    #[inline]
    pub fn stream_response(&self) -> &StreamResponse {
        &self.0
    }

    #[inline]
    pub fn into_stream_response(self) -> StreamResponse {
        self.0
    }

    #[inline]
    pub fn task(&self) -> Option<&Task> {
        self.0.task.as_ref()
    }

    #[inline]
    pub fn message(&self) -> Option<&Message> {
        self.0.message.as_ref()
    }

    #[inline]
    pub fn status_update(&self) -> Option<&TaskStatusUpdateEvent> {
        self.0.status_update.as_ref()
    }

    #[inline]
    pub fn artifact_update(&self) -> Option<&TaskArtifactUpdateEvent> {
        self.0.artifact_update.as_ref()
    }
}

impl TryFrom<StreamResponse> for ValidatedTaskChunk {
    type Error = BamlRtError;

    fn try_from(stream: StreamResponse) -> Result<Self, Self::Error> {
        if stream.task.is_none()
            && (stream.status_update.is_some() || stream.artifact_update.is_some())
        {
            return Err(BamlRtError::InvalidArgument(
                "status_update or artifact_update requires task in chunk".into(),
            ));
        }
        Ok(Self(stream))
    }
}

impl TryFrom<SendMessageResponse> for ValidatedTaskChunk {
    type Error = BamlRtError;

    fn try_from(response: SendMessageResponse) -> Result<Self, Self::Error> {
        StreamResponse {
            message: response.message,
            task: response.task,
            status_update: None,
            artifact_update: None,
            extra: response.extra,
        }
        .try_into()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use super::{StreamChunk, StreamChunkView, Task, TaskState, TaskStatus, TaskStatusUpdateEvent};

    #[test]
    fn stream_chunk_view_parses_task_id_from_nested_status_update_alias() {
        let chunk = json!({
            "statusUpdate": {
                "status_update": {
                    "contextId": "ctx-1-1",
                    "taskId": "a2a-child-123",
                    "status": { "state": "TASK_STATE_WORKING" }
                }
            }
        });
        let view = StreamChunkView::new(chunk);
        assert_eq!(view.task_id().map(|id| id.as_str()), Some("a2a-child-123"));
        assert_eq!(view.task_state(), Some("TASK_STATE_WORKING"));
    }

    #[test]
    fn stream_chunk_view_parses_task_id_from_string_task_payload() {
        let chunk = json!({
            "task": "{\"id\":\"a2a-child-789\",\"status\":{\"state\":\"TASK_STATE_SUBMITTED\"}}"
        });
        let view = StreamChunkView::new(chunk);
        assert_eq!(view.task_id().map(|id| id.as_str()), Some("a2a-child-789"));
        assert_eq!(view.task_state(), Some("TASK_STATE_SUBMITTED"));
    }

    #[test]
    fn stream_chunk_view_parses_task_id_from_message_task_id() {
        let view = StreamChunkView::new(json!({
            "message": {
                "taskId": "task-msg-1"
            }
        }));
        assert_eq!(view.task_id().map(|id| id.as_str()), Some("task-msg-1"));
    }

    #[test]
    fn stream_chunk_view_parses_task_id_from_top_level_status_update_alias() {
        let view = StreamChunkView::new(json!({
            "status_update": {
                "taskId": "task-snake-1",
                "status": { "state": "TASK_STATE_WORKING" }
            }
        }));
        assert_eq!(view.task_id().map(|id| id.as_str()), Some("task-snake-1"));
    }

    #[test]
    fn stream_chunk_view_parses_task_id_from_stringified_chunk_payload() {
        let view = StreamChunkView::new(json!({
            "chunk": "{\"statusUpdate\":{\"taskId\":\"task-nested-1\"}}"
        }));
        assert_eq!(view.task_id().map(|id| id.as_str()), Some("task-nested-1"));
    }

    #[test]
    fn stream_chunk_view_parses_task_id_from_tool_chunks_array() {
        let view = StreamChunkView::new(json!({
            "chunks": [
                {
                    "task": "{\"id\":\"task-array-1\"}",
                    "statusUpdate": "{\"taskId\":\"task-array-1\"}"
                }
            ]
        }));
        assert_eq!(view.task_id().map(|id| id.as_str()), Some("task-array-1"));
    }

    #[test]
    fn stream_chunk_view_task_id_parsing_stops_after_depth_cap() {
        let mut nested: Value = json!({ "taskId": "task-too-deep" });
        for _ in 0..10 {
            nested = Value::String(nested.to_string());
        }
        let view = StreamChunkView::new(nested);
        assert!(view.task_id().is_none());
    }

    #[test]
    fn stream_chunk_status_update_serializes_without_double_nesting() {
        let chunk = StreamChunk::status_update(TaskStatusUpdateEvent {
            task_id: Some(baml_rt_core::ids::TaskId::from_external(
                baml_rt_core::ids::ExternalId::new("task-123"),
            )),
            status: Some(TaskStatus {
                state: Some(TaskState::String("TASK_STATE_WORKING".to_string())),
                ..TaskStatus::default()
            }),
            ..TaskStatusUpdateEvent::default()
        });
        let value = serde_json::to_value(chunk).expect("serializes");
        assert_eq!(
            value
                .get("statusUpdate")
                .and_then(|v| v.get("taskId"))
                .and_then(Value::as_str),
            Some("task-123")
        );
        assert!(
            value
                .get("statusUpdate")
                .and_then(|v| v.get("statusUpdate"))
                .is_none()
        );
    }

    #[test]
    fn stream_chunk_task_serializes_without_double_nesting() {
        let chunk = StreamChunk::task(Task {
            id: Some(baml_rt_core::ids::TaskId::from_external(
                baml_rt_core::ids::ExternalId::new("task-abc"),
            )),
            context_id: None,
            artifacts: Vec::new(),
            history: Vec::new(),
            status: None,
            metadata: None,
            extra: HashMap::new(),
        });
        let value = serde_json::to_value(chunk).expect("serializes");
        assert_eq!(
            value
                .get("task")
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str),
            Some("task-abc")
        );
        assert!(value.get("task").and_then(|v| v.get("task")).is_none());
    }
}
