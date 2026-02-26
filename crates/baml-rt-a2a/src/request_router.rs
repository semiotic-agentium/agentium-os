use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Outcome, Result,
    bus::{A2aEffectMetadata, A2aLivenessRole, EffectEmitter, EffectEvent},
    context::InvocationScope,
    ids::AgentId,
    stream_completion::StreamCompletion,
};
use baml_rt_observability::{metrics, spans};
use baml_rt_quickjs::{
    QuickJSBridge,
    a2a_stream::{StreamOutput, spawn_stream_handover},
};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use crate::{a2a, a2a_types, handlers::TaskHandler, result_pipeline::ResultStoragePipeline};

/// Extracts task ID string from a normalized stream chunk (task.id, statusUpdate.taskId, or message.taskId).
fn task_id_from_chunk(value: &Value) -> Option<String> {
    value
        .get("task")
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            value
                .get("statusUpdate")
                .and_then(|s| s.get("taskId"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .or_else(|| {
            value
                .get("message")
                .and_then(|m| m.get("taskId"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

/// Builds a stream chunk for TASK_STATE_SUBMITTED so the client FSM sees SUBMITTED before WORKING.
/// This chunk is applied through the same pipeline as agent yields (store_result → apply_task_delta):
/// the task is created/recorded and SUBMITTED is written to the store and to provenance (task_created,
/// task_status_changed). So the wire and PROV stay aligned—we record the task.
fn make_submitted_chunk(context_id: &str, task_id: &str) -> Value {
    serde_json::json!({
        "statusUpdate": {
            "status": { "state": "TASK_STATE_SUBMITTED" },
            "taskId": task_id,
            "contextId": context_id
        },
        "task": {
            "id": task_id,
            "contextId": context_id,
            "status": { "state": "TASK_STATE_SUBMITTED" }
        }
    })
}

/// Non-stream invocation: waits for JS promise to resolve and returns the result.
///
/// **Conversation routing:** Each call receives the invocation scope for a single A2A request
/// (one conversation). The host ensures multiple concurrent conversations each get their own
/// scope; the handler is invoked with that scope so messages and yielded chunks stay with the
/// correct conversation.
#[async_trait]
pub trait JsInvoker: Send + Sync {
    async fn invoke_handler(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Value>;
    /// Stream invocation that yields chunks incrementally via the returned receiver.
    /// Each item is (raw_chunk, Option<StreamCompletion>); last item has Some(completion).
    /// When `resume_rx` is Some (live session), the collector blocks on InputRequired for true resume.
    /// When `relay_rx` is Some (live stream), the collector drains it each iteration so tool/status chunks stay in order.
    async fn invoke_stream_incremental(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
        resume_rx: Option<mpsc::Receiver<Value>>,
        relay_rx: Option<mpsc::Receiver<Value>>,
    ) -> Result<mpsc::Receiver<StreamOutput>>;
}

pub struct QuickJsInvoker {
    bridge: Arc<Mutex<QuickJSBridge>>,
}

impl QuickJsInvoker {
    pub fn new(bridge: Arc<Mutex<QuickJSBridge>>) -> Self {
        Self { bridge }
    }
}

#[async_trait]
impl JsInvoker for QuickJsInvoker {
    async fn invoke_handler(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Value> {
        let request = request.clone();
        let scope = scope.clone();
        let bridge = self.bridge.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                // If params serialization fails, surface the error instead of sending an empty payload.
                let js_request = a2a::request_to_js_value(&request)?;
                let mut bridge = bridge.lock().await;
                bridge
                    .invoke_js_function(&scope, "onChatMessage", js_request)
                    .await
            })
        })
        .await
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?
    }

    async fn invoke_stream_incremental(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
        resume_rx: Option<mpsc::Receiver<Value>>,
        relay_rx: Option<mpsc::Receiver<Value>>,
    ) -> Result<mpsc::Receiver<StreamOutput>> {
        let js_request = a2a::request_to_js_value(request)?;
        let rx = spawn_stream_handover(
            self.bridge.clone(),
            scope.clone(),
            js_request,
            resume_rx,
            relay_rx,
        )
        .await;
        Ok(rx)
    }
}

/// Optional resume channel for live stream: when Some, the collector blocks on InputRequired for true resume.
pub type ResumeChannel = Option<(mpsc::Sender<Value>, mpsc::Receiver<Value>)>;

#[async_trait]
pub trait RequestRouter: Send + Sync {
    /// Route the request. Scope is type-enforced: caller must pass the invocation scope (e.g. from transport).
    /// When `resume_channel` is Some (live session), stream can suspend on InputRequired and resume on next turn.
    /// When `relay_rx` is Some (live stream), collect path drains it and emits in order (single stream).
    async fn route(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
        resume_channel: ResumeChannel,
        relay_rx: Option<mpsc::Receiver<Value>>,
    ) -> Result<a2a::A2aOutcome>;
}

pub struct MethodBasedRouter {
    task_handler: Arc<dyn TaskHandler>,
    js_invoker: Arc<dyn JsInvoker>,
    result_pipeline: Arc<dyn ResultStoragePipeline>,
    effect_emitter: Arc<dyn EffectEmitter>,
    agent_id: AgentId,
}

impl MethodBasedRouter {
    pub fn new(
        task_handler: Arc<dyn TaskHandler>,
        js_invoker: Arc<dyn JsInvoker>,
        result_pipeline: Arc<dyn ResultStoragePipeline>,
        effect_emitter: Arc<dyn EffectEmitter>,
        agent_id: AgentId,
    ) -> Self {
        Self {
            task_handler,
            js_invoker,
            result_pipeline,
            effect_emitter,
            agent_id,
        }
    }
}

#[async_trait]
impl RequestRouter for MethodBasedRouter {
    async fn route(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
        resume_channel: ResumeChannel,
        relay_rx: Option<mpsc::Receiver<Value>>,
    ) -> Result<a2a::A2aOutcome> {
        match &request.params {
            a2a::A2aParams::TasksGet(req) => self.task_handler.handle_get(req.clone()).await,
            a2a::A2aParams::TasksList(req) => self.task_handler.handle_list(req.clone()).await,
            a2a::A2aParams::TasksSubscribe(req) => {
                self.task_handler
                    .handle_subscribe(req.clone(), request.invocation)
                    .await
            }
            _ => {
                let start = Instant::now();
                let context_id = scope.context_id().clone();
                let route_span = spans::a2a_route(request.method().as_str(), context_id.as_str());
                let _route_guard = route_span.enter();

                // Build metadata
                let mut metadata_map = serde_json::Map::new();
                if let Some(id) = request.id.as_ref() {
                    metadata_map.insert(
                        "request_id".to_string(),
                        serde_json::to_value(id).unwrap_or(Value::Null),
                    );
                }
                metadata_map.insert(
                    "message_id".to_string(),
                    Value::String(scope.message_id().as_str().to_string()),
                );
                metadata_map.insert(
                    "agent_id".to_string(),
                    Value::String(scope.agent_id().as_str().to_string()),
                );
                if let Some(task_id) = scope.task_id_opt() {
                    metadata_map.insert(
                        "task_id".to_string(),
                        Value::String(task_id.as_str().to_string()),
                    );
                }
                let metadata = Value::Object(metadata_map);

                let effect_metadata = A2aEffectMetadata {
                    agent_id: self.agent_id.clone(),
                    method: request.method().as_str().to_string(),
                    request_id: request.id.as_ref().and_then(|id| match id {
                        a2a_types::JSONRPCId::String(s) => Some(s.clone()),
                        a2a_types::JSONRPCId::Integer(n) => Some(n.to_string()),
                        a2a_types::JSONRPCId::Null => None,
                    }),
                    liveness_role: A2aLivenessRole::Command,
                    metadata: metadata.clone(),
                };

                // Emit A2A started
                if let Err(e) = self
                    .effect_emitter
                    .emit(EffectEvent::A2aStarted {
                        context_id: context_id.clone(),
                        metadata: effect_metadata.clone(),
                    })
                    .await
                {
                    tracing::warn!(error = ?e, "Failed to emit A2A effect started");
                }

                // Compute result so we always emit A2aCompleted on every exit (success or failure)
                let result = async {
                    let js_span =
                        spans::a2a_js_invoke(request.method().as_str(), request.invocation);
                    let _js_guard = js_span.enter();
                    if request.is_stream() {
                        // Incremental only: no internal collect. Outermost consumer (transport/SSE) drains the receiver.
                        let resume_tx = resume_channel
                            .as_ref()
                            .map(|(tx, _): &(mpsc::Sender<Value>, mpsc::Receiver<Value>)| tx.clone());
                        let resume_rx = resume_channel.map(|(_, rx)| rx);
                        let mut chunk_rx = self
                            .js_invoker
                            .invoke_stream_incremental(request, scope, resume_rx, relay_rx)
                            .await?;
                        let (tx, rx) = mpsc::channel(64);
                        let pipeline = self.result_pipeline.clone();
                        let scope = scope.clone();
                        let inject_submitted =
                            request.method() == a2a::A2aMethod::MessageSendStream;
                        tokio::spawn(async move {
                            let mut normalizer = a2a::JsChunkNormalizer::new(&scope);
                            let mut index = 0_usize;
                            let mut last_completion = None;
                            let mut submitted_sent = false;
                            while let Some(output) = chunk_rx.recv().await {
                                let (raw_chunk, completion, is_relay) = match &output {
                                    StreamOutput::Chunk(v) => (v.clone(), None, false),
                                    StreamOutput::RelayChunk(v) => (v.clone(), None, true),
                                    StreamOutput::Terminal(v, c) => (v.clone(), Some(*c), false),
                                };
                                last_completion = completion;
                                match normalizer.normalize_value(raw_chunk) {
                                    Ok(mut normalized) => {
                                        if is_relay
                                            && let Some(obj) = normalized.as_object_mut()
                                        {
                                            obj.insert(
                                                "__toolStreamChunk".to_string(),
                                                serde_json::Value::Bool(true),
                                            );
                                        }
                                        // Emit SUBMITTED first for client FSM; same pipeline so store and PROV record the task.
                                        if inject_submitted && !submitted_sent {
                                            let context_id_str = scope.context_id().as_str();
                                            let task_id_opt = scope
                                                .task_id_opt()
                                                .map(|t| t.as_str().to_string())
                                                .or_else(|| task_id_from_chunk(&normalized));
                                            if let Some(ref task_id_str) = task_id_opt {
                                                let submitted_chunk = make_submitted_chunk(
                                                    context_id_str,
                                                    task_id_str,
                                                );
                                                if pipeline
                                                    .store_result(&submitted_chunk)
                                                    .await
                                                    .is_ok()
                                                    && tx
                                                        .send((submitted_chunk, 0, None))
                                                        .await
                                                        .is_ok()
                                                {
                                                    submitted_sent = true;
                                                    index = 1;
                                                }
                                            }
                                        }
                                        if let Err(e) = pipeline.store_result(&normalized).await {
                                            tracing::warn!(
                                                error = %e,
                                                "stream: store_result failed for chunk; still forwarding to client"
                                            );
                                        }
                                        if tx.send((normalized, index, completion)).await.is_err() {
                                            break;
                                        }
                                        index += 1;
                                    }
                                    Err(e) => {
                                        if tx.send((
                                            serde_json::json!({"error": e.to_string()}),
                                            index,
                                            Some(StreamCompletion::SemanticFinal),
                                        ))
                                        .await
                                        .is_err()
                                        {
                                            tracing::debug!("stream error send failed (receiver dropped)");
                                        }
                                        break;
                                    }
                                }
                                // Only exit on terminal completions; InputRequired leaves the stream open for resume.
                                if completion
                                    .as_ref()
                                    .is_some_and(|c| c.is_wire_final())
                                {
                                    break;
                                }
                            }
                            if last_completion.is_none() && !tx.is_closed()
                                && tx.send((
                                    Value::Null,
                                    index,
                                    Some(StreamCompletion::ChannelClosed),
                                ))
                                .await
                                .is_err()
                            {
                                tracing::debug!("stream channel-closed send failed (receiver dropped)");
                            }
                        });
                        Ok(a2a::A2aOutcome::Stream(a2a::StreamHandle {
                            receiver: rx,
                            resume_tx,
                        }))
                    } else {
                        let mut normalizer = a2a::JsChunkNormalizer::new(scope);
                        let result = self.js_invoker.invoke_handler(request, scope).await?;
                        let normalized = normalizer.normalize_value(result)?;
                        self.result_pipeline.store_result(&normalized).await?;
                        Ok(a2a::A2aOutcome::Response(normalized))
                    }
                }
                .await;

                if let Err(err) = &result {
                    tracing::warn!(
                        method = %request.method().as_str(),
                        context_id = %context_id.as_str(),
                        task_id = ?scope.task_id_opt().map(|id| id.as_str().to_string()),
                        invocation = ?request.invocation,
                        error = ?err,
                        "a2a route failed at js/storage boundary"
                    );
                }

                let duration = start.elapsed();
                let duration_ms = duration.as_millis() as u64;
                let outcome = Outcome::from(result.is_ok());
                let mode = if request.is_stream() {
                    "stream"
                } else {
                    "non_stream"
                };
                let result_str = if outcome.is_success() {
                    "success"
                } else {
                    "error"
                };
                metrics::record_quickjs_invoke(mode, result_str, duration);
                if let Err(e) = self
                    .effect_emitter
                    .emit(EffectEvent::A2aCompleted {
                        context_id: context_id.clone(),
                        metadata: effect_metadata,
                        duration_ms,
                        outcome,
                    })
                    .await
                {
                    tracing::warn!(error = ?e, "Failed to emit A2A effect completed");
                }

                result
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // These unit tests use MockJsInvoker only. They do not exercise the real QuickJsInvoker
    // (which holds QuickJS bridge state and is !Send across awaits). The requirement that
    // the real invoker's futures be Send (so the router can run on a multi-threaded runtime
    // and be spawned) is enforced by integration tests that use the real A2aAgent with
    // tokio::runtime::Builder::new_multi_thread() and task spawn (e.g. run_handle_a2a_property_test
    // prop_interleaved_*, and quickjs_invoker_send_requirement test). If QuickJsInvoker is
    // changed so its async methods return !Send futures, those integration tests will fail
    // to compile.

    use async_trait::async_trait;
    use baml_rt_core::{InvocationKind, bus::BusWithEffects, stream_completion::StreamCompletion};
    use serde_json::json;

    use super::*;
    use crate::a2a_types::{GetTaskRequest, ListTasksRequest, SubscribeToTaskRequest};

    struct MockJsInvoker {
        stream_chunks: Vec<Value>,
    }

    #[async_trait]
    impl JsInvoker for MockJsInvoker {
        async fn invoke_handler(
            &self,
            _request: &a2a::A2aRequest,
            _scope: &InvocationScope,
        ) -> Result<Value> {
            Ok(Value::Null)
        }
        async fn invoke_stream_incremental(
            &self,
            _request: &a2a::A2aRequest,
            _scope: &InvocationScope,
            _resume_rx: Option<mpsc::Receiver<Value>>,
            _relay_rx: Option<mpsc::Receiver<Value>>,
        ) -> Result<mpsc::Receiver<StreamOutput>> {
            let (tx, rx) = mpsc::channel(64);
            let chunks = self.stream_chunks.clone();
            tokio::spawn(async move {
                for c in chunks {
                    let _ = tx.send(StreamOutput::Chunk(c)).await;
                }
                let _ = tx
                    .send(StreamOutput::Terminal(
                        Value::Null,
                        StreamCompletion::SemanticFinal,
                    ))
                    .await;
            });
            Ok(rx)
        }
    }

    struct NoopTaskHandler;

    #[async_trait]
    impl TaskHandler for NoopTaskHandler {
        async fn handle_get(&self, _req: GetTaskRequest) -> Result<a2a::A2aOutcome> {
            Err(BamlRtError::InvalidArgument("mock".to_string()))
        }
        async fn handle_list(&self, _req: ListTasksRequest) -> Result<a2a::A2aOutcome> {
            Err(BamlRtError::InvalidArgument("mock".to_string()))
        }
        async fn handle_subscribe(
            &self,
            _req: SubscribeToTaskRequest,
            _invocation: InvocationKind,
        ) -> Result<a2a::A2aOutcome> {
            Err(BamlRtError::InvalidArgument("mock".to_string()))
        }
    }

    struct NoopResultPipeline;

    #[async_trait::async_trait]
    impl ResultStoragePipeline for NoopResultPipeline {
        async fn store_result(&self, _value: &Value) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stream_request_uses_only_invoke_stream_chunks() {
        let chunk_a = json!({ "message": { "parts": [{ "text": "a" }] } });
        let chunk_b = json!({ "statusUpdate": { "status": { "state": "TASK_STATE_WORKING" } } });
        let invoker = Arc::new(MockJsInvoker {
            stream_chunks: vec![chunk_a.clone(), chunk_b.clone()],
        });
        let effect_emitter: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
        let agent_id = AgentId::from_uuid(baml_rt_core::ids::UuidId::new(uuid::Uuid::new_v4()));
        let router = MethodBasedRouter::new(
            Arc::new(NoopTaskHandler),
            invoker,
            Arc::new(NoopResultPipeline),
            effect_emitter,
            agent_id.clone(),
        );
        let request_value = json!({
            "jsonrpc": "2.0",
            "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-1",
                    "role": "ROLE_USER",
                    "parts": [{ "text": "hello" }]
                }
            },
            "id": "req-1"
        });
        let request = a2a::A2aRequest::from_value(request_value).unwrap();
        assert!(request.is_stream());

        let scope = InvocationScope::synthetic_message(agent_id.clone());
        let outcome = router.route(&request, &scope, None, None).await.unwrap();
        match outcome {
            a2a::A2aOutcome::Stream(handle) => {
                let mut rx = handle.receiver;
                let mut chunks: Vec<Value> = Vec::new();
                while let Some((v, _index, completion)) = rx.recv().await {
                    if completion.is_some() {
                        break;
                    }
                    if v != Value::Null {
                        chunks.push(v);
                    }
                }
                // message.sendStream injects TASK_STATE_SUBMITTED first so client FSM sees SUBMITTED → WORKING → …
                assert_eq!(chunks.len(), 3);
                assert_eq!(
                    chunks[0]
                        .get("statusUpdate")
                        .and_then(|s| s.get("status"))
                        .and_then(|s| s.get("state"))
                        .and_then(|v| v.as_str()),
                    Some("TASK_STATE_SUBMITTED")
                );
                assert_eq!(
                    chunks[1]
                        .get("message")
                        .and_then(|m| m.get("parts"))
                        .and_then(|p| p.get(0))
                        .and_then(|p| p.get("text"))
                        .and_then(|t| t.as_str()),
                    Some("a")
                );
                assert!(chunks[2].get("statusUpdate").is_some());
            }
            a2a::A2aOutcome::Response(_) => panic!("expected Stream outcome"),
        }
    }
}
