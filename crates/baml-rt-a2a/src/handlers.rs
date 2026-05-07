use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, InvocationKind, Result, stream_completion::StreamCompletion, to_json_value,
};
use baml_rt_quickjs::QuickJSBridge;
use tokio::sync::{Mutex, mpsc};

use crate::{
    a2a,
    a2a_store::{TaskEventRecorder, TaskRepository, TaskUpdateEvent, TaskUpdateQueue},
    a2a_types::{
        GetTaskRequest, ListTasksRequest, ListTasksResponse, StreamChunk, StreamResponse,
        SubscribeToTaskRequest, TaskStatusUpdateEvent,
    },
    events::EventEmitter,
};

const TASK_NOT_FOUND_MSG: &str = "Task not found";

#[async_trait]
pub trait TaskHandler: Send + Sync {
    async fn handle_get(&self, request: GetTaskRequest) -> Result<a2a::A2aOutcome>;
    async fn handle_list(&self, request: ListTasksRequest) -> Result<a2a::A2aOutcome>;
    async fn handle_subscribe(
        &self,
        request: SubscribeToTaskRequest,
        invocation: InvocationKind,
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

#[async_trait]
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
        invocation: InvocationKind,
    ) -> Result<a2a::A2aOutcome> {
        let task_id_str = request.id.as_str();
        let task = self.repository.get(task_id_str, None).await;
        if task.is_none() {
            tracing::debug!(
                task_id = task_id_str,
                "tasks.subscribe: task not found in repository"
            );
        }
        let task =
            task.ok_or_else(|| BamlRtError::InvalidArgument(TASK_NOT_FOUND_MSG.to_string()))?;
        let value = to_json_value(&task)?;

        if invocation.is_stream() {
            let mut responses = Vec::new();
            responses.push(to_json_value(&StreamChunk::task(task.clone()))?);
            if let Some(status_ev) = TaskStatusUpdateEvent::from_task_current_status(&task) {
                responses.push(to_json_value(&StreamChunk::status_update(status_ev))?);
            }

            for update in self.update_queue.drain_updates(request.id.as_str()).await {
                let chunk = match update {
                    TaskUpdateEvent::Status(internal) => StreamChunk::status_update(internal),
                    TaskUpdateEvent::Artifact(internal) => StreamChunk::artifact_update(internal),
                };
                responses.push(to_json_value(&chunk)?);
            }

            let n = responses.len();
            let (tx, rx) = mpsc::channel(64);
            for (i, chunk) in responses.into_iter().enumerate() {
                let sr = serde_json::from_value(chunk).unwrap_or_default();
                if tx.send((sr, i, None)).await.is_err() {
                    tracing::debug!("stream chunk send failed (receiver dropped)");
                }
            }
            if tx
                .send((
                    StreamResponse::default(),
                    n,
                    Some(StreamCompletion::SemanticFinal),
                ))
                .await
                .is_err()
            {
                tracing::debug!("stream final send failed (receiver dropped)");
            }
            drop(tx);
            Ok(a2a::A2aOutcome::Stream(a2a::StreamHandle {
                receiver: rx,
                resume_tx: None,
                abort_tx: None,
            }))
        } else {
            Ok(a2a::A2aOutcome::Response(value))
        }
    }
}
