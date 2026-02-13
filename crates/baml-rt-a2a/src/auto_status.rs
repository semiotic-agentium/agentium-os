use crate::a2a_store::{TaskStoreBackend, TaskUpdateEvent};
use crate::a2a_types::{
    A2aMessageId, Message, MessageRole, Part, ROLE_AGENT, TaskState, TaskStatus,
};
use async_trait::async_trait;
use baml_rt_core::effects::{EffectEvent, EffectSubscriber};
use baml_rt_core::ids::{ContextId, DerivedId, ExternalId, TaskId};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

static STATUS_MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(1);

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

    fn task_id_from_metadata(metadata: &Value) -> Option<TaskId> {
        let raw = metadata.get("task_id")?.as_str()?;
        Some(TaskId::from_external(ExternalId::new(raw.to_string())))
    }

    fn new_status_message(
        &self,
        context_id: &ContextId,
        task_id: &TaskId,
        text: String,
    ) -> Message {
        let counter = STATUS_MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let derived = DerivedId::new(format!(
            "a2a-host-status-{}-{}",
            context_id.as_str(),
            counter
        ));
        Message {
            message_id: A2aMessageId::outgoing(derived),
            role: MessageRole::String(ROLE_AGENT.to_string()),
            parts: vec![Part {
                text: Some(text),
                ..Part::default()
            }],
            context_id: Some(context_id.clone()),
            task_id: Some(task_id.clone()),
            reference_task_ids: Vec::new(),
            extensions: Vec::new(),
            metadata: None,
            extra: HashMap::new(),
        }
    }

    async fn emit_working(
        &self,
        context_id: &ContextId,
        task_id: TaskId,
        text: String,
    ) -> baml_rt_core::Result<()> {
        let message = self.new_status_message(context_id, &task_id, text);
        let status = TaskStatus {
            state: Some(TaskState::String("TASK_STATE_WORKING".to_string())),
            message: Some(message),
            timestamp: None,
            extra: HashMap::new(),
        };
        match self
            .task_store
            .record_status_update(Some(task_id.clone()), Some(context_id.clone()), status)
            .await
        {
            Ok(Some(event)) => {
                if let Err(err) = self.update_tx.send(event) {
                    tracing::debug!(error = ?err, "Auto WORKING status update dropped");
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
                let Some(task_id) = Self::task_id_from_metadata(&metadata.metadata) else {
                    return Ok(());
                };
                let text = format!("Invoking tool: {}", metadata.tool_name);
                if let Err(e) = self.emit_working(context_id, task_id, text).await {
                    tracing::debug!(error = ?e, "Auto WORKING status update (tool) failed");
                }
            }
            EffectEvent::LlmStarted {
                context_id,
                metadata,
            } => {
                let Some(task_id) = Self::task_id_from_metadata(&metadata.metadata) else {
                    return Ok(());
                };
                let text = format!(
                    "Calling model: {} ({})",
                    metadata.model, metadata.function_name
                );
                if let Err(e) = self.emit_working(context_id, task_id, text).await {
                    tracing::debug!(error = ?e, "Auto WORKING status update (llm) failed");
                }
            }
            _ => {}
        }
        Ok(())
    }
}
