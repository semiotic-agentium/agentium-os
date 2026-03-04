//! system/internal_a2a tool: session-based A2A conversation call.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{A2aRequestHandler, A2aWireRequest, Result, context, ids::TaskId};
use baml_rt_tools::{
    BundleName, ToolBundle, ToolBundleMetadata, ToolCapability, ToolFailure, ToolHandler,
    ToolSession, ToolSessionError, ToolStep,
    tools::{ToolFunctionMetadata, ToolSessionContext, validate_open_input},
};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::{
    metadata::system_internal_a2a_metadata,
    tools::{
        ConversationChunk, ConversationMessage, ConversationPart, InternalA2aCompletion,
        InternalA2aNextOutput, InternalA2aOpenInput, InternalA2aSendInput, InternalA2aTarget,
    },
};

/// System bundle exposing the system/internal_a2a tool.
pub struct A2aSessionBundle {
    handler: Arc<dyn A2aRequestHandler>,
}

impl A2aSessionBundle {
    pub fn new(handler: Arc<dyn A2aRequestHandler>) -> Self {
        Self { handler }
    }
}

impl ToolBundle for A2aSessionBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = BundleName::new("system".to_string()).expect("system bundle name must be valid");
        ToolBundleMetadata {
            name,
            description: "System tools (e.g. agent-to-agent session)".to_string(),
            config_schema: None,
            secret_requirements: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn ToolHandler>> {
        vec![Arc::new(A2aSessionToolHandler {
            handler: self.handler.clone(),
            metadata: system_internal_a2a_metadata(),
        })]
    }
}

struct A2aSessionToolHandler {
    handler: Arc<dyn A2aRequestHandler>,
    metadata: ToolFunctionMetadata,
}

#[async_trait]
impl ToolHandler for A2aSessionToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        validate_open_input::<InternalA2aOpenInput>(open_input.clone())?;
        let open: InternalA2aOpenInput =
            serde_json::from_value(open_input).map_err(baml_rt_core::BamlRtError::Json)?;
        Ok(Box::new(A2aSession {
            ctx,
            handler: self.handler.clone(),
            target: open.target,
            queue: VecDeque::new(),
            output_rx: None,
            stream_handle: None,
            seen_output: false,
            empty_stream_notice_emitted: false,
            closed: false,
        }))
    }
}

struct A2aSession {
    ctx: ToolSessionContext,
    handler: Arc<dyn A2aRequestHandler>,
    target: InternalA2aTarget,
    queue: VecDeque<InternalA2aNextOutput>,
    output_rx: Option<async_channel::Receiver<InternalA2aNextOutput>>,
    /// JoinHandle for the task that consumes the A2A stream. Aborted in Drop so the task
    /// does not outlive the session and trigger "context is being shutdown" panics.
    stream_handle: Option<JoinHandle<()>>,
    seen_output: bool,
    empty_stream_notice_emitted: bool,
    closed: bool,
}

fn parse_send_input(input: Value) -> std::result::Result<Vec<ConversationPart>, String> {
    match serde_json::from_value::<InternalA2aSendInput>(input) {
        Ok(InternalA2aSendInput {
            parts: Some(parts),
            text: None,
        }) if !parts.is_empty() => Ok(parts),
        Ok(InternalA2aSendInput {
            parts: None,
            text: Some(text),
        }) => Ok(vec![ConversationPart {
            text: Some(text),
            ..Default::default()
        }]),
        Ok(InternalA2aSendInput {
            parts: Some(_),
            text: Some(_),
        }) => Err("system/internal_a2a input must set exactly one of parts or text".to_string()),
        Ok(InternalA2aSendInput {
            parts: Some(_),
            text: None,
        }) => Err("system/internal_a2a input.parts must not be empty".to_string()),
        Ok(_) => Err("system/internal_a2a input must set exactly one of parts or text".to_string()),
        Err(err) => Err(format!(
            "Invalid system/internal_a2a input: expected {{ parts: [...] }} or {{ text: string }} ({err})"
        )),
    }
}

fn build_send_stream_request(
    parts: Vec<ConversationPart>,
    target: &InternalA2aTarget,
    context_id: &baml_rt_core::ids::ContextId,
    parent_task_id: Option<&TaskId>,
) -> Value {
    let child_task_id = format!("a2a-child-{}", uuid::Uuid::new_v4());
    let reference_task_ids = parent_task_id
        .map(|task_id| vec![task_id.as_str().to_string()])
        .unwrap_or_default();
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "message.sendStream",
        "id": serde_json::Value::Null,
        "params": {
            "message": {
                "messageId": format!("system-a2a-{}", uuid::Uuid::new_v4()),
                "role": "ROLE_USER",
                "parts": parts,
                "contextId": context_id.as_str(),
                "taskId": child_task_id,
                "referenceTaskIds": reference_task_ids
            },
            "metadata": {
                "target": {
                    "agent_package": target.agent_package,
                    "agent_instance_id": target.agent_instance_id
                }
            }
        }
    })
}

fn current_parent_task_id() -> Option<TaskId> {
    context::current_scope()
        .ok()
        .and_then(|scope| scope.task_id_opt().cloned())
}

fn extract_chunk_value(value: Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value;
    };

    if let Some(result) = obj.get("result").and_then(|v| v.as_object()) {
        if let Some(chunk) = result.get("chunk") {
            return chunk.clone();
        }
        if result
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && let Some(chunk) = result.get("chunk")
        {
            return chunk.clone();
        }
    }

    if let Some(chunk) = obj.get("chunk") {
        return chunk.clone();
    }

    value
}

fn to_conversation_chunk(value: Value) -> ConversationChunk {
    let candidate = extract_chunk_value(value);
    if let Some(obj) = candidate.as_object() {
        let message = obj.get("message").and_then(|msg| {
            let role = msg
                .as_object()
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let parts = msg
                .as_object()
                .and_then(|m| m.get("parts"))
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| serde_json::from_value::<ConversationPart>(p.clone()).ok())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if parts.is_empty() {
                None
            } else {
                Some(ConversationMessage { role, parts })
            }
        });

        let task = obj.get("task").map(|v| v.to_string());
        let status_update = obj.get("statusUpdate").map(|v| v.to_string());
        let artifact_update = obj.get("artifactUpdate").map(|v| v.to_string());
        if message.is_some()
            || task.is_some()
            || status_update.is_some()
            || artifact_update.is_some()
        {
            return ConversationChunk {
                message,
                task,
                status_update,
                artifact_update,
            };
        }
    }

    ConversationChunk {
        message: Some(ConversationMessage {
            role: None,
            parts: vec![ConversationPart {
                text: Some(candidate.to_string()),
                ..Default::default()
            }],
        }),
        task: None,
        status_update: None,
        artifact_update: None,
    }
}

fn chunk_value_has_input_required(value: &Value) -> bool {
    let state = value
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("statusUpdate")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.get("state"))
                .and_then(Value::as_str)
        });
    matches!(state, Some("TASK_STATE_INPUT_REQUIRED"))
}

fn merge_outputs(outputs: Vec<InternalA2aNextOutput>) -> InternalA2aNextOutput {
    let mut chunks = Vec::new();
    let mut completion = None;
    for out in outputs {
        chunks.extend(out.chunks);
        if matches!(out.completion, Some(InternalA2aCompletion::InputRequired)) {
            completion = Some(InternalA2aCompletion::InputRequired);
        }
    }
    InternalA2aNextOutput { chunks, completion }
}

#[async_trait]
impl ToolSession for A2aSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.closed {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "A2A session {session_id} is closed",
                session_id = self.ctx.session_id
            ))));
        }
        let parts = parse_send_input(input)
            .map_err(|msg| ToolSessionError::Tool(ToolFailure::invalid_input(msg)))?;
        if self.output_rx.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "system/internal_a2a session: send only valid once after open".to_string(),
            )));
        }
        let parent_task_id = current_parent_task_id();
        let request = build_send_stream_request(
            parts,
            &self.target,
            &self.ctx.context_id,
            parent_task_id.as_ref(),
        );
        let handler = self.handler.clone();
        let (tx, rx) = async_channel::unbounded::<InternalA2aNextOutput>();
        self.output_rx = Some(rx);
        let handle = tokio::spawn(async move {
            match handler
                .handle_a2a_stream(A2aWireRequest::from(request))
                .await
            {
                Ok(stream) => {
                    futures_util::pin_mut!(stream);
                    while let Some(response) = stream.next().await {
                        let value = response.into_inner();
                        let chunk = extract_chunk_value(value.clone());
                        let input_required = chunk_value_has_input_required(&chunk);
                        let out = InternalA2aNextOutput {
                            chunks: vec![to_conversation_chunk(value)],
                            completion: if input_required {
                                Some(InternalA2aCompletion::InputRequired)
                            } else {
                                None
                            },
                        };
                        if tx.send(out).await.is_err() {
                            break;
                        }
                        if input_required {
                            break;
                        }
                    }
                }
                Err(err) => {
                    let fallback = InternalA2aNextOutput {
                        chunks: vec![ConversationChunk {
                            message: Some(ConversationMessage {
                                role: None,
                                parts: vec![ConversationPart {
                                    text: Some(format!(
                                        "system/internal_a2a execution failed: {err}"
                                    )),
                                    ..Default::default()
                                }],
                            }),
                            task: None,
                            status_update: None,
                            artifact_update: None,
                        }],
                        completion: None,
                    };
                    let _ = tx.send(fallback).await;
                }
            }
            tx.close();
        });
        self.stream_handle = Some(handle);
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        if let Some(output) = self.queue.pop_front() {
            let mut batch = vec![output];
            while let Some(next) = self.queue.pop_front() {
                batch.push(next);
            }
            let merged = merge_outputs(batch);
            let value = serde_json::to_value(&merged).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                    "Invalid A2A output: {error}",
                    error = e
                )))
            })?;
            let step = if matches!(
                merged.completion,
                Some(InternalA2aCompletion::InputRequired)
            ) {
                ToolStep::Suspended { output: value }
            } else {
                ToolStep::Streaming { output: value }
            };
            self.seen_output = true;
            return Ok(step);
        }
        if let Some(rx) = &self.output_rx {
            match rx.recv().await {
                Ok(output) => {
                    let mut batch = vec![output];
                    while let Ok(next) = rx.try_recv() {
                        batch.push(next);
                    }
                    let merged = merge_outputs(batch);
                    let value = serde_json::to_value(&merged).map_err(|e| {
                        ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                            "Invalid A2A output: {error}",
                            error = e
                        )))
                    })?;
                    let step = if matches!(
                        merged.completion,
                        Some(InternalA2aCompletion::InputRequired)
                    ) {
                        ToolStep::Suspended { output: value }
                    } else {
                        ToolStep::Streaming { output: value }
                    };
                    self.seen_output = true;
                    return Ok(step);
                }
                Err(_) => {
                    self.output_rx = None;
                    if !self.seen_output && !self.empty_stream_notice_emitted {
                        self.empty_stream_notice_emitted = true;
                        let out = InternalA2aNextOutput {
                            chunks: vec![ConversationChunk {
                                message: Some(ConversationMessage {
                                    role: None,
                                    parts: vec![ConversationPart {
                                        text: Some(
                                            "system/internal_a2a stream ended without output"
                                                .to_string(),
                                        ),
                                        ..Default::default()
                                    }],
                                }),
                                task: None,
                                status_update: None,
                                artifact_update: None,
                            }],
                            completion: None,
                        };
                        let value = serde_json::to_value(&out).map_err(|e| {
                            ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                                "Invalid A2A output: {error}",
                                error = e
                            )))
                        })?;
                        return Ok(ToolStep::Streaming { output: value });
                    }
                }
            }
        }
        Ok(ToolStep::Done { output: None })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }
}

impl Drop for A2aSession {
    fn drop(&mut self) {
        if let Some(h) = self.stream_handle.take() {
            h.abort();
        }
    }
}
