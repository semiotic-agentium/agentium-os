use std::collections::HashMap;

use baml_rt_core::ids::{ArtifactId, ContextId, DerivedId, ExternalId, MessageId, TaskId};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageRole {
    String(String),
    Integer(i64),
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
        task: Task,
        #[serde(flatten)]
        extra: HashMap<String, Value>,
    },
    StatusUpdate {
        status_update: TaskStatusUpdateEvent,
        #[serde(flatten)]
        extra: HashMap<String, Value>,
    },
    ArtifactUpdate {
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
    /// Build a typed view from the wire chunk. Parses task_id and task_state once; use accessors thereafter.
    pub fn new(value: Value) -> Self {
        let task_id = Self::parse_task_id(&value);
        let task_state = Self::parse_task_state(&value);
        let has_status_update = value.get("statusUpdate").is_some();
        let has_artifact_update = value.get("artifactUpdate").is_some();
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

    fn parse_task_id(v: &Value) -> Option<TaskId> {
        let from_val = |val: &Value| val.get("id").and_then(Value::as_str).map(String::from);
        let id_str = v.get("task").and_then(from_val).or_else(|| {
            v.get("statusUpdate")
                .and_then(|s| s.get("taskId"))
                .and_then(Value::as_str)
                .map(String::from)
        })?;
        Some(TaskId::from_external(ExternalId::new(id_str)))
    }

    fn parse_task_state(v: &Value) -> Option<String> {
        let state_from = |val: &Value| {
            val.get("status")
                .and_then(|s| s.get("state"))
                .and_then(|s| s.as_str())
                .map(String::from)
        };
        v.get("task").and_then(state_from).or_else(|| {
            let su = v.get("statusUpdate")?;
            let ev = if su.get("status").is_some() {
                su
            } else {
                su.get("statusUpdate").or_else(|| su.get("status_update"))?
            };
            state_from(ev)
        })
    }

    pub fn is_null(&self) -> bool {
        self.raw.is_null()
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
