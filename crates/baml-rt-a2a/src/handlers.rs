use crate::a2a;
use crate::a2a_store::{TaskEventRecorder, TaskRepository, TaskUpdateEvent, TaskUpdateQueue};
use crate::a2a_types::{
    GetTaskRequest, ListTasksRequest, ListTasksResponse, StreamChunk, SubscribeToTaskRequest,
    TaskStatusUpdateEvent,
};
use crate::events::EventEmitter;
use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result, to_json_value};
use baml_rt_quickjs::QuickJSBridge;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const TASK_NOT_FOUND_MSG: &str = "Task not found";

#[async_trait(?Send)]
pub trait TaskHandler: Send + Sync {
    async fn handle_get(&self, request: GetTaskRequest) -> Result<a2a::A2aOutcome>;
    async fn handle_list(&self, request: ListTasksRequest) -> Result<a2a::A2aOutcome>;
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

    /// Shared task event recorder (e.g. for custom routing or tests).
    pub fn recorder(&self) -> &Arc<dyn TaskEventRecorder> {
        &self.recorder
    }

    /// Shared QuickJS bridge (e.g. for custom invoke paths or tests).
    pub fn bridge(&self) -> &Arc<Mutex<QuickJSBridge>> {
        &self.bridge
    }

    /// Shared event emitter (e.g. for pushing updates from handler code).
    pub fn emitter(&self) -> &Arc<dyn EventEmitter> {
        &self.emitter
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
            .ok_or_else(|| BamlRtError::InvalidArgument(TASK_NOT_FOUND_MSG.to_string()))?;
        let value = to_json_value(&task)?;
        Ok(a2a::A2aOutcome::Response(value))
    }

    async fn handle_list(&self, request: ListTasksRequest) -> Result<a2a::A2aOutcome> {
        let response: ListTasksResponse = self.repository.list(&request).await;
        let value = to_json_value(&response)?;
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
            .ok_or_else(|| BamlRtError::InvalidArgument(TASK_NOT_FOUND_MSG.to_string()))?;
        let value = to_json_value(&task)?;

        if is_stream {
            let mut responses = Vec::new();
            let task_chunk = StreamChunk::Task {
                task: task.clone(),
                extra: HashMap::new(),
            };
            responses.push(to_json_value(&task_chunk)?);
            if let Some(ref status) = task.status {
                let status_update = TaskStatusUpdateEvent {
                    context_id: task.context_id.clone(),
                    task_id: task.id.clone(),
                    status: Some(status.clone()),
                    metadata: None,
                    extra: HashMap::new(),
                };
                let status_chunk = StreamChunk::StatusUpdate {
                    status_update,
                    extra: HashMap::new(),
                };
                responses.push(to_json_value(&status_chunk)?);
            }

            for update in self.update_queue.drain_updates(request.id.as_str()).await {
                let chunk = match update {
                    TaskUpdateEvent::Status(internal) => StreamChunk::StatusUpdate {
                        status_update: internal,
                        extra: HashMap::new(),
                    },
                    TaskUpdateEvent::Artifact(internal) => StreamChunk::ArtifactUpdate {
                        artifact_update: internal,
                        extra: HashMap::new(),
                    },
                };
                responses.push(to_json_value(&chunk)?);
            }

            Ok(a2a::A2aOutcome::Stream(responses))
        } else {
            Ok(a2a::A2aOutcome::Response(value))
        }
    }
}
