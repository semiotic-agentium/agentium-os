// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
    a2a_types::{A2aMessageId, Message, MessageRole, Part, TaskState, TaskStatus},
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
        role: MessageRole::Agent,
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

/// Emits `TASK_STATE_WORKING` task status updates derived from tool invocations.
///
/// These updates are **notification-only** (no new provenance): they reflect effects that are
/// already recorded elsewhere, and exist to improve client UX via `tasks.subscribe`.
/// LLM start notices are intentionally excluded so conversation history only picks up
/// actual tool-call headers rather than internal model-session banners.
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
            .record_status_update(task_id.clone(), context_id.clone(), status)
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
    fn name(&self) -> &'static str {
        "auto_status"
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        if let EffectEvent::ToolStarted {
            context_id,
            metadata,
        } = event
        {
            let Some(task_id) = task_id_from_metadata(&metadata.metadata) else {
                return Ok(());
            };
            let text = format!("Invoking tool: {}", metadata.tool_name);
            let meta = Some(working_status_metadata_tool(&metadata.tool_name));
            if let Err(e) = self.emit_working(context_id, task_id, text, meta).await {
                tracing::debug!(error = ?e, "Auto WORKING status update (tool) failed");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use baml_rt_core::{
        Result,
        bus::{LlmEffectMetadata, ToolEffectMetadata, ToolNameResolution},
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;
    use crate::{
        a2a_store::{TaskChunkApplier, TaskEventRecorder, TaskRepository},
        a2a_types::{
            Artifact, ListTasksRequest, ListTasksResponse, Task, TaskStatus, ValidatedTaskChunk,
        },
    };

    #[derive(Default)]
    struct RecordingTaskStore {
        recorded_statuses: Mutex<Vec<(TaskId, ContextId, TaskStatus)>>,
    }

    impl RecordingTaskStore {
        async fn recorded_statuses(&self) -> Vec<(TaskId, ContextId, TaskStatus)> {
            self.recorded_statuses.lock().await.clone()
        }
    }

    #[async_trait]
    impl TaskRepository for RecordingTaskStore {
        async fn upsert(&self, _task: Task) -> Result<Option<Task>> {
            Ok(None)
        }

        async fn ensure_task_exists(
            &self,
            _task_id: &TaskId,
            _context_id: Option<&ContextId>,
        ) -> Result<()> {
            Ok(())
        }

        async fn get(&self, _id: &str, _history_length: Option<usize>) -> Option<Task> {
            None
        }

        async fn list(&self, _request: &ListTasksRequest) -> ListTasksResponse {
            ListTasksResponse {
                tasks: Vec::new(),
                next_page_token: None,
                total_size: None,
                page_size: None,
                extra: HashMap::new(),
            }
        }

        async fn cancel(&self, _id: &str) -> Option<Task> {
            None
        }

        async fn insert_message(&self, _message: &crate::a2a_types::Message) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl TaskEventRecorder for RecordingTaskStore {
        async fn record_status_update(
            &self,
            task_id: TaskId,
            context_id: ContextId,
            status: TaskStatus,
        ) -> Result<Option<TaskUpdateEvent>> {
            self.recorded_statuses
                .lock()
                .await
                .push((task_id, context_id, status));
            Ok(None)
        }

        async fn record_artifact_update(
            &self,
            _task_id: TaskId,
            _context_id: ContextId,
            _artifact: Artifact,
            _append: Option<bool>,
            _last_chunk: Option<bool>,
        ) -> Result<Option<TaskUpdateEvent>> {
            Ok(None)
        }
    }

    #[async_trait]
    impl TaskChunkApplier for RecordingTaskStore {
        async fn apply_task_chunk(
            &self,
            _chunk: ValidatedTaskChunk,
        ) -> Result<Vec<TaskUpdateEvent>> {
            Ok(Vec::new())
        }
    }
    #[tokio::test]
    async fn auto_status_emits_tool_headers_but_skips_llm_headers() {
        let store = Arc::new(RecordingTaskStore::default());
        let (update_tx, _update_rx) = broadcast::channel(8);
        let subscriber = AutoWorkingStatusSubscriber::new(store.clone(), update_tx);
        let context_id = ContextId::new(7, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-7".to_string()));

        subscriber
            .on_effect(&EffectEvent::LlmStarted {
                context_id: context_id.clone(),
                metadata: LlmEffectMetadata {
                    client: "openai".to_string(),
                    model: "gpt-4o-mini".to_string(),
                    function_name: "ClassifyCoordinatorTurn".to_string(),
                    prompt: json!({}),
                    metadata: json!({
                        "task_id": task_id.as_str(),
                    }),
                    tool_name: ToolNameResolution::NotApplicable,
                },
            })
            .await
            .expect("llm started handled");

        assert!(
            store.recorded_statuses().await.is_empty(),
            "LLM start notices must not be materialized as durable status history"
        );

        subscriber
            .on_effect(&EffectEvent::ToolStarted {
                context_id: context_id.clone(),
                metadata: ToolEffectMetadata {
                    tool_name: "system/discover_tools".to_string(),
                    function_name: None,
                    args: json!({}),
                    metadata: json!({
                        "task_id": task_id.as_str(),
                    }),
                    delegation_target: None,
                    tool_backend: None,
                    tool_digest: None,
                },
            })
            .await
            .expect("tool started handled");

        let recorded = store.recorded_statuses().await;
        assert_eq!(
            recorded.len(),
            1,
            "tool starts must still emit one WORKING status"
        );
        let (recorded_task_id, recorded_context_id, status) = &recorded[0];
        assert_eq!(recorded_task_id, &task_id);
        assert_eq!(recorded_context_id, &context_id);
        let text = status
            .message
            .as_ref()
            .and_then(|message| message.parts.first())
            .and_then(|part| part.text.as_deref());
        assert_eq!(text, Some("Invoking tool: system/discover_tools"));
    }
}
