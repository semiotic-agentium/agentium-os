use baml_rt_core::Outcome;
use baml_rt_core::ids::{AgentId, ArtifactId, ContextId, EventId, MessageId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn next_event_id() -> EventId {
    let id = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    EventId::from_counter(id)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct AgentType(String);

impl AgentType {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return None;
        }
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LlmUsage {
    Known {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    },
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CallScope {
    Message { message_id: MessageId },
    Task { task_id: TaskId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProvEventData {
    LlmCallStarted {
        scope: CallScope,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: Value,
    },
    LlmCallCompleted {
        scope: CallScope,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: Value,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
    },
    ToolCallStarted {
        scope: CallScope,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: Value,
    },
    ToolCallCompleted {
        scope: CallScope,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: Value,
        duration_ms: u64,
        outcome: Outcome,
    },
    AgentBooted {
        agent_id: AgentId,
        agent_type: AgentType,
        agent_version: String,
        archive_path: String,
    },
    TaskCreated {
        task_id: TaskId,
        agent_id: AgentId,
    },
    TaskStatusChanged {
        task_id: TaskId,
        old_status: Option<String>,
        new_status: Option<String>,
    },
    TaskArtifactGenerated {
        task_id: TaskId,
        artifact_id: Option<ArtifactId>,
        artifact_type: Option<String>,
    },
    MessageReceived {
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
    },
    MessageSent {
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScopedEvent {
    pub id: EventId,
    pub context_id: ContextId,
    pub task_id: TaskId,
    pub timestamp_ms: u64,
    pub data: ProvEventData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEvent {
    pub id: EventId,
    pub context_id: ContextId,
    pub timestamp_ms: u64,
    pub data: ProvEventData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProvEvent {
    Task(TaskScopedEvent),
    Global(GlobalEvent),
}

impl ProvEvent {
    pub fn id(&self) -> &EventId {
        match self {
            ProvEvent::Task(event) => &event.id,
            ProvEvent::Global(event) => &event.id,
        }
    }

    pub fn context_id(&self) -> &ContextId {
        match self {
            ProvEvent::Task(event) => &event.context_id,
            ProvEvent::Global(event) => &event.context_id,
        }
    }

    pub fn task_id(&self) -> Option<&TaskId> {
        match self {
            ProvEvent::Task(event) => Some(&event.task_id),
            ProvEvent::Global(_) => None,
        }
    }

    pub fn timestamp_ms(&self) -> u64 {
        match self {
            ProvEvent::Task(event) => event.timestamp_ms,
            ProvEvent::Global(event) => event.timestamp_ms,
        }
    }

    pub fn data(&self) -> &ProvEventData {
        match self {
            ProvEvent::Task(event) => &event.data,
            ProvEvent::Global(event) => &event.data,
        }
    }

    pub fn llm_call_started_global(
        context_id: ContextId,
        message_id: MessageId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: Value,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallStarted {
                scope: CallScope::Message { message_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
            },
        })
    }

    pub fn llm_call_started_task(
        context_id: ContextId,
        task_id: TaskId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: Value,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallStarted {
                scope: CallScope::Task { task_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_global(
        context_id: ContextId,
        message_id: MessageId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: Value,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message { message_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
                usage,
                duration_ms,
                outcome,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn llm_call_completed_task(
        context_id: ContextId,
        task_id: TaskId,
        client: String,
        model: String,
        function_name: String,
        prompt: Value,
        metadata: Value,
        usage: LlmUsage,
        duration_ms: u64,
        outcome: Outcome,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Task { task_id },
                client,
                model,
                function_name,
                prompt,
                metadata,
                usage,
                duration_ms,
                outcome,
            },
        })
    }

    pub fn tool_call_started_global(
        context_id: ContextId,
        message_id: MessageId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: Value,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallStarted {
                scope: CallScope::Message { message_id },
                tool_name,
                function_name,
                args,
                metadata,
            },
        })
    }

    pub fn tool_call_started_task(
        context_id: ContextId,
        task_id: TaskId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: Value,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallStarted {
                scope: CallScope::Task { task_id },
                tool_name,
                function_name,
                args,
                metadata,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_completed_global(
        context_id: ContextId,
        message_id: MessageId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: Value,
        duration_ms: u64,
        outcome: Outcome,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallCompleted {
                scope: CallScope::Message { message_id },
                tool_name,
                function_name,
                args,
                metadata,
                duration_ms,
                outcome,
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_completed_task(
        context_id: ContextId,
        task_id: TaskId,
        tool_name: String,
        function_name: Option<String>,
        args: Value,
        metadata: Value,
        duration_ms: u64,
        outcome: Outcome,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::ToolCallCompleted {
                scope: CallScope::Task { task_id },
                tool_name,
                function_name,
                args,
                metadata,
                duration_ms,
                outcome,
            },
        })
    }

    pub fn agent_booted(
        context_id: ContextId,
        agent_id: AgentId,
        agent_type: AgentType,
        agent_version: String,
        archive_path: String,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms: now_millis(),
            data: ProvEventData::AgentBooted {
                agent_id,
                agent_type,
                agent_version,
                archive_path,
            },
        })
    }

    pub fn task_created(context_id: ContextId, task_id: TaskId, agent_id: AgentId) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskCreated { task_id, agent_id },
        })
    }

    pub fn task_status_changed(
        context_id: ContextId,
        task_id: TaskId,
        old_status: Option<String>,
        new_status: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskStatusChanged {
                task_id,
                old_status,
                new_status,
            },
        })
    }

    pub fn task_artifact_generated(
        context_id: ContextId,
        task_id: TaskId,
        artifact_id: Option<ArtifactId>,
        artifact_type: Option<String>,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id: task_id.clone(),
            timestamp_ms: now_millis(),
            data: ProvEventData::TaskArtifactGenerated {
                task_id,
                artifact_id,
                artifact_type,
            },
        })
    }

    pub fn message_received_task(
        context_id: ContextId,
        task_id: TaskId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id,
            timestamp_ms,
            data: ProvEventData::MessageReceived {
                id,
                role,
                content,
                metadata,
            },
        })
    }

    pub fn message_received_global(
        context_id: ContextId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms,
            data: ProvEventData::MessageReceived {
                id,
                role,
                content,
                metadata,
            },
        })
    }

    pub fn message_sent_task(
        context_id: ContextId,
        task_id: TaskId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Task(TaskScopedEvent {
            id: next_event_id(),
            context_id,
            task_id,
            timestamp_ms,
            data: ProvEventData::MessageSent {
                id,
                role,
                content,
                metadata,
            },
        })
    }

    pub fn message_sent_global(
        context_id: ContextId,
        id: MessageId,
        role: String,
        content: Vec<String>,
        metadata: Option<HashMap<String, String>>,
        timestamp_ms: u64,
    ) -> Self {
        ProvEvent::Global(GlobalEvent {
            id: next_event_id(),
            context_id,
            timestamp_ms,
            data: ProvEventData::MessageSent {
                id,
                role,
                content,
                metadata,
            },
        })
    }
}
