use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber},
    ids::{ContextId, DerivedId, ExternalId, TaskId},
};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::{
    a2a_store::{TaskStoreBackend, TaskUpdateEvent},
    a2a_types::{A2aMessageId, Message, MessageRole, Part, ROLE_AGENT, TaskState, TaskStatus},
};

static STATUS_MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Extract task_id from effect metadata (used by AutoWorkingStatusSubscriber and LiveStreamWorkingRelay).
pub(crate) fn task_id_from_metadata(metadata: &Value) -> Option<TaskId> {
    let raw = metadata.get("task_id")?.as_str()?;
    Some(TaskId::from_external(ExternalId::new(raw.to_string())))
}

/// Optional metadata for WORKING status (e.g. to indicate tool vs LLM).
pub(crate) fn working_status_metadata_tool(tool_name: &str) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("kind".to_string(), Value::String("tool".to_string()));
    m.insert("toolName".to_string(), Value::String(tool_name.to_string()));
    m
}

/// Build a TASK_STATE_WORKING status event for relay to task stream (notification-only).
/// When `metadata` is provided (e.g. from [`working_status_metadata_tool`]), clients can
/// identify tool-call vs LLM-call WORKING chunks without parsing message text.
/// `task_id` may be None for message-scope streams; relay still sends WORKING so clients see tool activity.
pub(crate) fn make_working_status_event(
    context_id: &ContextId,
    task_id: Option<&TaskId>,
    text: String,
    metadata: Option<HashMap<String, Value>>,
) -> crate::a2a_types::TaskStatusUpdateEvent {
    let counter = STATUS_MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let derived = DerivedId::new(format!(
        "a2a-host-status-{}-{}",
        context_id.as_str(),
        counter
    ));
    let message = Message {
        message_id: A2aMessageId::outgoing(derived),
        role: MessageRole::String(ROLE_AGENT.to_string()),
        parts: vec![Part {
            text: Some(text),
            ..Part::default()
        }],
        context_id: Some(context_id.clone()),
        task_id: task_id.cloned(),
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    };
    let status = TaskStatus {
        state: Some(TaskState::String("TASK_STATE_WORKING".to_string())),
        message: Some(message),
        timestamp: None,
        extra: HashMap::new(),
    };
    crate::a2a_types::TaskStatusUpdateEvent {
        context_id: Some(context_id.clone()),
        task_id: task_id.cloned(),
        status: Some(status),
        metadata,
        extra: HashMap::new(),
    }
}

/// Emits `TASK_STATE_WORKING` task status updates derived from tool/LLM invocations.
///
/// These updates are **notification-only** (no new provenance): they reflect effects that are
/// already recorded elsewhere, and exist to improve client UX via `tasks.subscribe`.
pub struct AutoWorkingStatusSubscriber {
    task_store: Arc<dyn TaskStoreBackend>,
    update_tx: broadcast::Sender<TaskUpdateEvent>,
}

impl AutoWorkingStatusSubscriber {
    pub fn new(
        task_store: Arc<dyn TaskStoreBackend>,
        update_tx: broadcast::Sender<TaskUpdateEvent>,
    ) -> Self {
        Self {
            task_store,
            update_tx,
        }
    }

    async fn emit_working(
        &self,
        context_id: &ContextId,
        task_id: TaskId,
        text: String,
        metadata: Option<HashMap<String, Value>>,
    ) -> baml_rt_core::Result<()> {
        let status_ev = make_working_status_event(context_id, Some(&task_id), text, metadata);
        let status = status_ev
            .status
            .clone()
            .expect("make_working_status_event sets status");
        match self
            .task_store
            .record_status_update(task_id.clone(), Some(context_id.clone()), status)
            .await
        {
            Ok(Some(event)) => {
                if self.update_tx.send(event).is_err() {
                    tracing::debug!("Broadcast send failed (no receivers)");
                }
            }
            Ok(None) => {
                tracing::debug!(
                    task_id = %task_id.as_str(),
                    context_id = %context_id.as_str(),
                    attempted_state = "TASK_STATE_WORKING",
                    reason = "fsm_rejected",
                    "Auto WORKING status update rejected by FSM"
                );
            }
            Err(e) => {
                tracing::debug!(
                    error = ?e,
                    task_id = %task_id.as_str(),
                    context_id = %context_id.as_str(),
                    "Auto WORKING status update failed"
                );
            }
        }
        Ok(())
    }
}

#[async_trait]
impl EffectSubscriber for AutoWorkingStatusSubscriber {
    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        match event {
            EffectEvent::ToolStarted {
                context_id,
                metadata,
            } => {
                let Some(task_id) = task_id_from_metadata(&metadata.metadata) else {
                    return Ok(());
                };
                let text = format!("Invoking tool: {}", metadata.tool_name);
                let meta = Some(working_status_metadata_tool(&metadata.tool_name));
                if let Err(e) = self.emit_working(context_id, task_id, text, meta).await {
                    tracing::debug!(error = ?e, "Auto WORKING status update (tool) failed");
                }
            }
            EffectEvent::LlmStarted {
                context_id,
                metadata,
            } => {
                let Some(task_id) = task_id_from_metadata(&metadata.metadata) else {
                    return Ok(());
                };
                let text = format!(
                    "Calling model: {} ({})",
                    metadata.model, metadata.function_name
                );
                if let Err(e) = self.emit_working(context_id, task_id, text, None).await {
                    tracing::debug!(error = ?e, "Auto WORKING status update (llm) failed");
                }
            }
            _ => {}
        }
        Ok(())
    }
}
