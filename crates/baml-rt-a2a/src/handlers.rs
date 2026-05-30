// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, InvocationKind, Result, stream_completion::StreamCompletion, to_json_value,
};
use baml_rt_provenance::{ReplayError, TaskGraphReader};
use baml_rt_quickjs::QuickJSBridge;
use futures_util::{StreamExt, stream::BoxStream};
use tokio::sync::{Mutex, mpsc};

use crate::{
    a2a,
    a2a_store::{TaskEventRecorder, TaskRepository, TaskUpdateEvent, status_to_string},
    a2a_types::{
        GetTaskRequest, ListTasksRequest, ListTasksResponse, StreamChunk, StreamResponse,
        SubscribeToTaskRequest, Task, TaskStatusUpdateEvent,
    },
    events::EventEmitter,
    task_update_broadcaster::{TaskUpdateBroadcaster, TaskUpdateFrame},
    task_update_drain::{drain_replay_into_events, frame_to_task_update_event},
    task_update_session::{TaskUpdateReplaySource, TaskUpdateSession},
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
    task_graph_reader: Arc<dyn TaskGraphReader>,
    task_update_broadcaster: Arc<TaskUpdateBroadcaster>,
    bridge: Arc<Mutex<QuickJSBridge>>,
    emitter: Arc<dyn EventEmitter>,
}

impl DefaultTaskHandler {
    pub fn new(
        repository: Arc<dyn TaskRepository>,
        recorder: Arc<dyn TaskEventRecorder>,
        task_graph_reader: Arc<dyn TaskGraphReader>,
        task_update_broadcaster: Arc<TaskUpdateBroadcaster>,
        bridge: Arc<Mutex<QuickJSBridge>>,
        emitter: Arc<dyn EventEmitter>,
    ) -> Self {
        Self {
            repository,
            recorder,
            task_graph_reader,
            task_update_broadcaster,
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

struct GraphReplaySource {
    reader: Arc<dyn TaskGraphReader>,
}

impl GraphReplaySource {
    fn new(reader: Arc<dyn TaskGraphReader>) -> Self {
        Self { reader }
    }
}

#[async_trait]
impl TaskUpdateReplaySource for GraphReplaySource {
    async fn replay_since(
        &self,
        scoped: baml_rt_provenance::metamodel::ScopedTaskRef,
        since: Option<baml_rt_provenance::TaskReplayCursor>,
    ) -> std::result::Result<
        BoxStream<'static, std::result::Result<TaskUpdateFrame, ReplayError>>,
        ReplayError,
    > {
        let mut replay = self
            .reader
            .replay_since(scoped, since)
            .await
            .map_err(ReplayError::from)?;
        let mut frames = Vec::new();
        while let Some(item) = replay.next().await {
            frames.push(item);
        }
        Ok(stream! {
            for item in frames {
                yield item;
            }
        }
        .boxed())
    }
}

fn task_is_terminal(task: &Task) -> bool {
    matches!(
        task.status.as_ref().and_then(status_to_string).as_deref(),
        Some(
            "TASK_STATE_COMPLETED"
                | "TASK_STATE_FAILED"
                | "TASK_STATE_CANCELED"
                | "TASK_STATE_REJECTED"
        )
    )
}

async fn send_subscribe_chunk(
    tx: &mpsc::Sender<(StreamResponse, usize, Option<StreamCompletion>)>,
    index: &mut usize,
    chunk: StreamChunk,
) -> bool {
    let value = match to_json_value(&chunk) {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(error = ?error, "failed to serialize subscribe chunk");
            return false;
        }
    };
    let sr = serde_json::from_value(value).unwrap_or_default();
    let current = *index;
    *index += 1;
    tx.send((sr, current, None)).await.is_ok()
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
            let scoped = self
                .task_graph_reader
                .resolve_by_task_id(&request.id)
                .await
                .map_err(|source| BamlRtError::InvalidArgumentWithSource {
                    message: format!(
                        "failed to resolve task {} for subscribe",
                        request.id.as_str()
                    ),
                    source: Box::new(source),
                })?
                .ok_or_else(|| BamlRtError::InvalidArgument(TASK_NOT_FOUND_MSG.to_string()))?;
            let replay_source: Arc<dyn TaskUpdateReplaySource> =
                Arc::new(GraphReplaySource::new(self.task_graph_reader.clone()));
            let mut session = TaskUpdateSession::open(
                self.task_update_broadcaster.as_ref(),
                replay_source,
                scoped,
                None,
            )
            .await
            .map_err(|source| BamlRtError::InvalidArgumentWithSource {
                message: format!(
                    "failed to open task update session for {}",
                    request.id.as_str()
                ),
                source: Box::new(source),
            })?;

            let terminal_snapshot = task_is_terminal(&task);
            let (tx, rx) = mpsc::channel(64);
            tokio::spawn(async move {
                let mut index = 0usize;
                if !send_subscribe_chunk(&tx, &mut index, StreamChunk::task(task.clone())).await {
                    return;
                }
                if let Some(status_ev) = TaskStatusUpdateEvent::from_task_current_status(&task)
                    && !send_subscribe_chunk(&tx, &mut index, StreamChunk::status_update(status_ev))
                        .await
                {
                    return;
                }

                match drain_replay_into_events(&mut session).await {
                    Ok(events) => {
                        for update in events {
                            let chunk = match update {
                                TaskUpdateEvent::Status(internal) => {
                                    StreamChunk::status_update(internal)
                                }
                                TaskUpdateEvent::Artifact(internal) => {
                                    StreamChunk::artifact_update(internal)
                                }
                            };
                            if !send_subscribe_chunk(&tx, &mut index, chunk).await {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::debug!(error = ?error, "task replay drain failed");
                    }
                }

                if !terminal_snapshot {
                    loop {
                        match session.next().await {
                            Ok(Some(frame)) => {
                                let Some(update) = frame_to_task_update_event(frame) else {
                                    continue;
                                };
                                let chunk = match update {
                                    TaskUpdateEvent::Status(internal) => {
                                        StreamChunk::status_update(internal)
                                    }
                                    TaskUpdateEvent::Artifact(internal) => {
                                        StreamChunk::artifact_update(internal)
                                    }
                                };
                                if !send_subscribe_chunk(&tx, &mut index, chunk).await {
                                    return;
                                }
                            }
                            Ok(None) => break,
                            Err(error) => {
                                tracing::debug!(error = ?error, "task subscribe live session failed");
                                break;
                            }
                        }
                    }
                }

                if tx
                    .send((
                        StreamResponse::default(),
                        index,
                        Some(StreamCompletion::SemanticFinal),
                    ))
                    .await
                    .is_err()
                {
                    tracing::debug!("stream final send failed (receiver dropped)");
                }
            });
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
