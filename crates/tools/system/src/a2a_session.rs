//! system/internal_a2a tool: session-based A2A conversation call.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, A2aWireRequest, Result, context,
    ids::{TaskId, UuidId},
    is_history_infrastructure_notice,
};
use baml_rt_tools::{
    BundleName, ToolBundle, ToolBundleMetadata, ToolCapability, ToolFailure, ToolHandler,
    ToolSession, ToolSessionError, ToolStep, opaque_json_map_from_object,
    tools::{
        HistoryContextSessionOp, HistoryContextStatus, HistoryContextV1, ToolFunctionMetadata,
        ToolSessionContext, validate_open_input,
    },
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
            secret_requests: Vec::new(),
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

    fn describe_invocation(&self, content: &Value) -> String {
        let step = content.get("step").unwrap_or(content);
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return "delegated agent: call".to_string(),
        };
        match op {
            "Open" => {
                if let Some(open) = step
                    .get("input")
                    .and_then(|v| serde_json::from_value::<InternalA2aOpenInput>(v.clone()).ok())
                {
                    format!("delegating to agent '{}'", open.target.agent_package)
                } else {
                    "delegating to agent".to_string()
                }
            }
            "Send" => {
                if let Some(send) = step
                    .get("input")
                    .and_then(|v| serde_json::from_value::<InternalA2aSendInput>(v.clone()).ok())
                {
                    let text = send.parts.first().and_then(|p| p.text.as_deref());
                    match text {
                        Some(t) if t.len() > 60 => {
                            format!("sending message to delegated agent: '{}...'", &t[..57])
                        }
                        Some(t) => format!("sending message to delegated agent: '{t}'"),
                        None => "sending message to delegated agent".to_string(),
                    }
                } else {
                    "sending message to delegated agent".to_string()
                }
            }
            "SearchRead" | "PageRead" => "reading delegated agent output".to_string(),
            "Finish" => "finished delegated agent session".to_string(),
            "Abort" => "aborted delegated agent session".to_string(),
            other => format!("delegated agent: {other}"),
        }
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
            child_task_id: TaskId::for_delegated_child(UuidId::new(uuid::Uuid::new_v4())),
            queue: VecDeque::new(),
            output_rx: None,
            stream_handle: None,
            seen_output: false,
            empty_stream_notice_emitted: false,
            closed: false,
            read_hop: 0,
            accumulated_outputs: Vec::new(),
        }))
    }
}

struct A2aSession {
    ctx: ToolSessionContext,
    handler: Arc<dyn A2aRequestHandler>,
    target: InternalA2aTarget,
    /// Stable delegated child task id for this tool session; reused across send/resume turns.
    child_task_id: TaskId,
    queue: VecDeque<InternalA2aNextOutput>,
    output_rx: Option<async_channel::Receiver<InternalA2aNextOutput>>,
    /// JoinHandle for the task that consumes the A2A stream. Aborted in Drop so the task
    /// does not outlive the session and trigger "context is being shutdown" panics.
    stream_handle: Option<JoinHandle<()>>,
    seen_output: bool,
    empty_stream_notice_emitted: bool,
    closed: bool,
    read_hop: u32,
    /// Accumulates all Streaming outputs across Read hops so the final Done hop can carry
    /// the full conversation payload into provenance.
    accumulated_outputs: Vec<InternalA2aNextOutput>,
}

fn parse_send_input(raw: Value) -> std::result::Result<Vec<ConversationPart>, String> {
    if raw.is_null() {
        return Err(
            "Invalid system/internal_a2a Send input: expected { parts: [{ text: '...' }] } — got null".to_string()
        );
    }
    // Accept legacy { text: "..." } shorthand and convert to parts.
    if let Some(text) = raw.get("text").and_then(|v| v.as_str())
        && !text.trim().is_empty()
    {
        return Ok(vec![ConversationPart {
            text: Some(text.to_string()),
            ..Default::default()
        }]);
    }
    match serde_json::from_value::<InternalA2aSendInput>(raw) {
        Ok(send) if !send.parts.is_empty() => Ok(send.parts),
        Ok(_) => Err("system/internal_a2a Send input.parts must not be empty".to_string()),
        Err(err) => Err(format!(
            "Invalid system/internal_a2a Send input: expected {{ parts: [{{ text: '...' }}] }} ({err})"
        )),
    }
}

/// Derive a child context id for inter-agent traffic so the called agent's
/// inbound `Message` provenance does NOT land on the caller's user-facing
/// conversation context. Stable across hops in the same delegated child task.
///
/// Format: `a2a:<caller_ctx>:<pkg>/<inst>:<child_task_id>`. The `a2a:` prefix
/// lets readers (UI picker, conversation projection) skip these contexts when
/// surfacing human-visible chats. Including the child task id prevents
/// concurrent delegated sessions to the same target from colliding on one
/// responder-side stream key.
fn derive_a2a_child_context_id(
    caller_context_id: &baml_rt_core::ids::ContextId,
    target: &InternalA2aTarget,
    child_task_id: &TaskId,
) -> baml_rt_core::ids::ContextId {
    baml_rt_core::ids::ContextId::for_a2a_child(
        caller_context_id,
        &target.agent_package,
        &target.agent_instance_id,
        child_task_id,
    )
}

fn build_send_stream_request(
    parts: Vec<ConversationPart>,
    target: &InternalA2aTarget,
    caller_context_id: &baml_rt_core::ids::ContextId,
    child_task_id: &TaskId,
    parent_task_id: Option<&TaskId>,
) -> Value {
    let reference_task_ids = parent_task_id
        .map(|task_id| vec![task_id.as_str().to_string()])
        .unwrap_or_default();
    let child_context_id = derive_a2a_child_context_id(caller_context_id, target, child_task_id);
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "message.sendStream",
        "id": serde_json::Value::Null,
        "params": {
            "message": {
                "messageId": format!("system-a2a-{id}", id = uuid::Uuid::new_v4()),
                "role": "ROLE_USER",
                "parts": parts,
                "contextId": child_context_id.as_str(),
                "taskId": child_task_id.as_str(),
                "referenceTaskIds": reference_task_ids
            },
            "metadata": {
                "kind": "agent-to-agent",
                "callerContextId": caller_context_id.as_str(),
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

    if let Some(result) = obj.get("result").and_then(|v| v.as_object())
        && let Some(chunk) = result.get("chunk")
    {
        return chunk.clone();
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

/// Returns true when a chunk carries actual conversational content from the agent.
///
/// Filters out infrastructure noise: task-state transitions, status updates, and
/// model/tool invocation notices ("Calling model: ...", "Invoking tool: ...").
/// Only chunks with a non-empty message part that isn't a system notice are kept.
fn is_conversational_chunk(chunk: &ConversationChunk) -> bool {
    let Some(ref message) = chunk.message else {
        return false;
    };
    message.parts.iter().any(|part| {
        part.text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .filter(|t| !is_history_infrastructure_notice(t))
            .is_some()
    })
}

fn merge_outputs(outputs: Vec<InternalA2aNextOutput>) -> InternalA2aNextOutput {
    let mut chunks = Vec::new();
    let mut completion = None;
    for out in outputs {
        chunks.extend(out.chunks);
        match out.completion {
            Some(InternalA2aCompletion::InputRequired) => {
                completion = Some(InternalA2aCompletion::InputRequired);
            }
            Some(InternalA2aCompletion::Failed)
                if !matches!(completion, Some(InternalA2aCompletion::InputRequired)) =>
            {
                completion = Some(InternalA2aCompletion::Failed);
            }
            Some(InternalA2aCompletion::Done) if completion.is_none() => {
                completion = Some(InternalA2aCompletion::Done);
            }
            _ => {}
        }
    }
    InternalA2aNextOutput {
        chunks,
        completion,
        history_context: None,
    }
}

fn completion_failure_message(output: &InternalA2aNextOutput) -> String {
    for chunk in &output.chunks {
        if let Some(message) = &chunk.message {
            for part in &message.parts {
                if let Some(text) = &part.text {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
        }
    }
    "system/internal_a2a stream failed".to_string()
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
        self.seen_output = false;
        self.empty_stream_notice_emitted = false;
        if let Some(rx) = &self.output_rx
            && rx.is_closed()
            && rx.is_empty()
        {
            self.output_rx = None;
        }
        if self.output_rx.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "system/internal_a2a session: send only valid once after open".to_string(),
            )));
        }
        if let Some(handle) = self.stream_handle.take()
            && !handle.is_finished()
        {
            handle.abort();
        }
        let parent_task_id = current_parent_task_id();
        let request = build_send_stream_request(
            parts,
            &self.target,
            &self.ctx.context_id,
            &self.child_task_id,
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
                            history_context: None,
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
                        completion: Some(InternalA2aCompletion::Failed),
                        history_context: None,
                    };
                    if tx.send(fallback).await.is_err() {
                        tracing::warn!(
                            "system/internal_a2a failed to emit synthetic error chunk (receiver dropped)"
                        );
                    }
                }
            }
            tx.close();
        });
        self.stream_handle = Some(handle);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        if let Some(output) = self.queue.pop_front() {
            let mut batch = vec![output];
            while let Some(next) = self.queue.pop_front() {
                batch.push(next);
            }
            let merged = merge_outputs(batch);
            self.read_hop = self.read_hop.saturating_add(1);
            let mut merged = merged;
            merged.history_context = Some(HistoryContextV1 {
                hop: self.read_hop,
                op: HistoryContextSessionOp::PageRead,
                status: match merged.completion {
                    Some(InternalA2aCompletion::InputRequired) => HistoryContextStatus::Suspended,
                    Some(InternalA2aCompletion::Failed) => HistoryContextStatus::Error,
                    _ => HistoryContextStatus::Streaming,
                },
                truncated: false,
                cursor: None,
                payload: Some(opaque_json_map_from_object(serde_json::json!({
                    "chunkCount": merged.chunks.len(),
                    "completion": merged.completion.as_ref().map(|c| format!("{:?}", c)),
                }))),
            });
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
                self.output_rx = None;
                ToolStep::Suspended { output: value }
            } else if matches!(merged.completion, Some(InternalA2aCompletion::Failed)) {
                self.output_rx = None;
                ToolStep::Error {
                    error: ToolFailure::execution_failed(completion_failure_message(&merged)),
                }
            } else {
                self.accumulated_outputs.push(merged.clone());
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
                    self.read_hop = self.read_hop.saturating_add(1);
                    let mut merged = merged;
                    merged.history_context = Some(HistoryContextV1 {
                        hop: self.read_hop,
                        op: HistoryContextSessionOp::PageRead,
                        status: match merged.completion {
                            Some(InternalA2aCompletion::InputRequired) => {
                                HistoryContextStatus::Suspended
                            }
                            Some(InternalA2aCompletion::Failed) => HistoryContextStatus::Error,
                            _ => HistoryContextStatus::Streaming,
                        },
                        truncated: false,
                        cursor: None,
                        payload: Some(opaque_json_map_from_object(serde_json::json!({
                            "chunkCount": merged.chunks.len(),
                            "completion": merged.completion.as_ref().map(|c| format!("{:?}", c)),
                        }))),
                    });
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
                        self.output_rx = None;
                        ToolStep::Suspended { output: value }
                    } else if matches!(merged.completion, Some(InternalA2aCompletion::Failed)) {
                        self.output_rx = None;
                        ToolStep::Error {
                            error: ToolFailure::execution_failed(completion_failure_message(
                                &merged,
                            )),
                        }
                    } else {
                        self.accumulated_outputs.push(merged.clone());
                        ToolStep::Streaming { output: value }
                    };
                    self.seen_output = true;
                    return Ok(step);
                }
                Err(_) => {
                    self.output_rx = None;
                    if !self.seen_output && !self.empty_stream_notice_emitted {
                        self.empty_stream_notice_emitted = true;
                        self.read_hop = self.read_hop.saturating_add(1);
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
                            history_context: Some(HistoryContextV1 {
                                hop: self.read_hop,
                                op: HistoryContextSessionOp::PageRead,
                                status: HistoryContextStatus::Streaming,
                                truncated: false,
                                cursor: None,
                                payload: Some(opaque_json_map_from_object(serde_json::json!({
                                    "chunkCount": 1,
                                    "completion": null
                                }))),
                            }),
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
        // Carry the conversation payload into the Done hop for provenance.
        // Strip infrastructure noise (task state, model/tool invocation notices) so
        // only actual agent messages reach the tool_result archive.
        let final_output = if self.accumulated_outputs.is_empty() {
            None
        } else {
            let mut merged = merge_outputs(std::mem::take(&mut self.accumulated_outputs));
            merged.chunks.retain(is_conversational_chunk);
            if merged.chunks.is_empty() {
                None
            } else {
                serde_json::to_value(&merged).ok()
            }
        };
        Ok(ToolStep::Done {
            output: final_output,
        })
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

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{ContextId, ExternalId};

    use super::*;

    fn responder_target() -> InternalA2aTarget {
        InternalA2aTarget {
            agent_package: "responder-agent".to_string(),
            agent_instance_id: "default".to_string(),
        }
    }

    #[test]
    fn build_send_stream_request_uses_stable_child_task_id() {
        let target = responder_target();
        let context_id = ContextId::new(1, 1);
        let child_task_id = TaskId::from_external(ExternalId::new("a2a-child-fixed".to_string()));
        let parts = vec![ConversationPart {
            text: Some("hello".to_string()),
            ..Default::default()
        }];

        let first =
            build_send_stream_request(parts.clone(), &target, &context_id, &child_task_id, None);
        let second = build_send_stream_request(parts, &target, &context_id, &child_task_id, None);

        assert_eq!(
            first
                .pointer("/params/message/taskId")
                .and_then(serde_json::Value::as_str),
            Some(child_task_id.as_str())
        );
        assert_eq!(
            second
                .pointer("/params/message/taskId")
                .and_then(serde_json::Value::as_str),
            Some(child_task_id.as_str())
        );
        assert_eq!(
            first.pointer("/params/message/contextId"),
            second.pointer("/params/message/contextId"),
            "the same delegated child task must reuse the same child context across hops"
        );
    }

    /// Regression for Bug B: inter-agent A2A sends MUST NOT propagate the caller's
    /// user-facing context id, otherwise `system-a2a-<uuid>` rows leak onto the human
    /// conversation as `direction=received` user turns.
    #[test]
    fn build_send_stream_request_isolates_inter_agent_context_id() {
        let target = responder_target();
        let caller_ctx = ContextId::new(1_700_000_000_000, 7);
        let child_task_id = TaskId::from_external(ExternalId::new("a2a-child".to_string()));
        let parts = vec![ConversationPart {
            text: Some("delegated query".to_string()),
            ..Default::default()
        }];

        let req = build_send_stream_request(parts, &target, &caller_ctx, &child_task_id, None);

        let wire_ctx = req
            .pointer("/params/message/contextId")
            .and_then(serde_json::Value::as_str)
            .expect("contextId on wire");
        assert_ne!(
            wire_ctx,
            caller_ctx.as_str(),
            "inter-agent send must not reuse caller's user-facing contextId"
        );
        assert!(
            wire_ctx.starts_with("a2a:"),
            "derived child context must use a2a: prefix so downstream readers can filter; got {wire_ctx}"
        );
        assert!(
            wire_ctx.contains(caller_ctx.as_str()),
            "derived context should embed caller for traceability; got {wire_ctx}"
        );
        assert!(
            wire_ctx.contains(&target.agent_package),
            "derived context should embed target package; got {wire_ctx}"
        );
        assert!(
            wire_ctx.contains(child_task_id.as_str()),
            "derived context should embed child task id to avoid parallel collisions; got {wire_ctx}"
        );

        let kind = req
            .pointer("/params/metadata/kind")
            .and_then(serde_json::Value::as_str);
        assert_eq!(kind, Some("agent-to-agent"));

        let caller_meta = req
            .pointer("/params/metadata/callerContextId")
            .and_then(serde_json::Value::as_str);
        assert_eq!(caller_meta, Some(caller_ctx.as_str()));
    }

    /// Derivation must be stable so that multi-hop A2A turns within the same child
    /// task land on the SAME child context id (otherwise the called agent loses
    /// continuity across turns), while different child tasks must not collide.
    #[test]
    fn derive_a2a_child_context_id_is_stable_per_child_task() {
        let target = responder_target();
        let caller = ContextId::new(42, 1);
        let child_task_id = TaskId::from_external(ExternalId::new("a2a-child-fixed".to_string()));
        let a = derive_a2a_child_context_id(&caller, &target, &child_task_id);
        let b = derive_a2a_child_context_id(&caller, &target, &child_task_id);
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn derive_a2a_child_context_id_differs_for_parallel_child_tasks() {
        let target = responder_target();
        let caller = ContextId::new(42, 1);
        let child_a = TaskId::from_external(ExternalId::new("a2a-child-a".to_string()));
        let child_b = TaskId::from_external(ExternalId::new("a2a-child-b".to_string()));
        let a = derive_a2a_child_context_id(&caller, &target, &child_a);
        let b = derive_a2a_child_context_id(&caller, &target, &child_b);
        assert_ne!(a.as_str(), b.as_str());
    }
}
