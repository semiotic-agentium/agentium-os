use crate::a2a;
use crate::a2a_store::{TaskEventRecorder, TaskRepository, TaskUpdateEvent, TaskUpdateQueue};
use crate::a2a_types::{
    CancelTaskRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse, StreamResponse,
    SubscribeToTaskRequest, TaskStatusUpdateEvent,
};
use crate::events::EventEmitter;
use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_quickjs::QuickJSBridge;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[async_trait(?Send)]
pub trait TaskHandler: Send + Sync {
    async fn handle_get(&self, request: GetTaskRequest) -> Result<a2a::A2aOutcome>;
    async fn handle_list(&self, request: ListTasksRequest) -> Result<a2a::A2aOutcome>;
    async fn handle_cancel(&self, request: CancelTaskRequest) -> Result<a2a::A2aOutcome>;
    async fn handle_subscribe(
        &self,
        request: SubscribeToTaskRequest,
        is_stream: bool,
    ) -> Result<a2a::A2aOutcome>;
}

pub struct DefaultTaskHandler {
    repository: Arc<dyn TaskRepository>,
    recorder: Arc<dyn TaskEventRecorder>,
    update_queue: Arc<dyn TaskUpdateQueue>,
    bridge: Arc<Mutex<QuickJSBridge>>,
    emitter: Arc<dyn EventEmitter>,
}

impl DefaultTaskHandler {
    pub fn new(
        repository: Arc<dyn TaskRepository>,
        recorder: Arc<dyn TaskEventRecorder>,
        update_queue: Arc<dyn TaskUpdateQueue>,
        bridge: Arc<Mutex<QuickJSBridge>>,
        emitter: Arc<dyn EventEmitter>,
    ) -> Self {
        Self {
            repository,
            recorder,
            update_queue,
            bridge,
            emitter,
        }
    }
}

#[async_trait(?Send)]
impl TaskHandler for DefaultTaskHandler {
    async fn handle_get(&self, request: GetTaskRequest) -> Result<a2a::A2aOutcome> {
        let history_length = request.history_length.and_then(|value| value.as_usize());
        let task = self
            .repository
            .get(request.id.as_str(), history_length)
            .await
            .ok_or_else(|| BamlRtError::InvalidArgument("Task not found".to_string()))?;
        let value = serde_json::to_value(task).map_err(BamlRtError::Json)?;
        Ok(a2a::A2aOutcome::Response(value))
    }

    async fn handle_list(&self, request: ListTasksRequest) -> Result<a2a::A2aOutcome> {
        let response: ListTasksResponse = self.repository.list(&request).await;
        let value = serde_json::to_value(response).map_err(BamlRtError::Json)?;
        Ok(a2a::A2aOutcome::Response(value))
    }

    async fn handle_cancel(&self, request: CancelTaskRequest) -> Result<a2a::A2aOutcome> {
        let task = {
            let task = self
                .repository
                .cancel(request.id.as_str())
                .await
                .ok_or_else(|| BamlRtError::InvalidArgument("Task not found".to_string()))?;
            if let Some(status) = task.status.clone()
                && let Some(event) = self
                    .recorder
                    .record_status_update(task.id.clone(), task.context_id.clone(), status)
                    .await
            {
                self.emitter.emit(event).await;
            }
            task
        };

        {
            let mut bridge = self.bridge.lock().await;
            let _ = bridge
                .invoke_optional_js_function(
                    "handle_a2a_cancel",
                    serde_json::to_value(&request).map_err(BamlRtError::Json)?,
                )
                .await?;
        }

        let value = serde_json::to_value(task).map_err(BamlRtError::Json)?;
        Ok(a2a::A2aOutcome::Response(value))
    }

    async fn handle_subscribe(
        &self,
        request: SubscribeToTaskRequest,
        is_stream: bool,
    ) -> Result<a2a::A2aOutcome> {
        let task = self
            .repository
            .get(request.id.as_str(), None)
            .await
            .ok_or_else(|| BamlRtError::InvalidArgument("Task not found".to_string()))?;
        let value = serde_json::to_value(&task).map_err(BamlRtError::Json)?;

        if is_stream {
            let mut responses = Vec::new();
            let status_update = task.status.as_ref().map(|status| TaskStatusUpdateEvent {
                context_id: task.context_id.clone(),
                task_id: task.id.clone(),
                status: Some(status.clone()),
                metadata: None,
                extra: HashMap::new(),
            });
            let response = StreamResponse {
                task: Some(task),
                status_update,
                message: None,
                artifact_update: None,
                extra: HashMap::new(),
            };
            responses.push(serde_json::to_value(response).map_err(BamlRtError::Json)?);

            for update in self.update_queue.drain_updates(request.id.as_str()).await {
                let stream_response = match update {
                    TaskUpdateEvent::Status(status_update) => StreamResponse {
                        status_update: Some(status_update),
                        message: None,
                        task: None,
                        artifact_update: None,
                        extra: HashMap::new(),
                    },
                    TaskUpdateEvent::Artifact(artifact_update) => StreamResponse {
                        artifact_update: Some(artifact_update),
                        message: None,
                        task: None,
                        status_update: None,
                        extra: HashMap::new(),
                    },
                };
                responses.push(serde_json::to_value(stream_response).map_err(BamlRtError::Json)?);
            }

            Ok(a2a::A2aOutcome::Stream(responses))
        } else {
            Ok(a2a::A2aOutcome::Response(value))
        }
    }
}
