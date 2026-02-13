use crate::a2a;
use crate::a2a_types;
use crate::handlers::TaskHandler;
use crate::result_pipeline::ResultStoragePipeline;
use async_trait::async_trait;
use baml_rt_core::context::InvocationScope;
use baml_rt_core::effects::{A2aEffectMetadata, EffectEmitter, EffectEvent};
use baml_rt_core::ids::AgentId;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_observability::{metrics, spans};
use baml_rt_quickjs::QuickJSBridge;
use baml_rt_quickjs::begin_a2a_yield_session;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// Non-stream invocation: waits for JS promise to resolve and returns the result.
///
/// **Conversation routing:** Each call receives the invocation scope for a single A2A request
/// (one conversation). The host ensures multiple concurrent conversations each get their own
/// scope; the handler is invoked with that scope so messages and yielded chunks stay with the
/// correct conversation.
#[async_trait(?Send)]
pub trait JsInvoker: Send + Sync {
    async fn invoke_handler(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Value>;
    /// Stream invocation: promise never resolves (CG4). Use only for stream requests.
    async fn invoke_stream(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Vec<Value>>;
}

/// **CG4 (Stream Promise Non-Resolution):** Invokes stream handlers without waiting for promise resolution.
/// The promise from `onChatMessage()` is designed to never resolve; chunks are collected via the yield buffer.
///
/// **Conversation routing:** Invocation uses the given `scope` (one per request). Multiple parallel
/// conversations are supported: each request gets its own scope from the transport, and chunks
/// are normalized with that scope so they are attributed to the correct conversation.
#[async_trait(?Send)]
pub trait JsStreamInvoker: Send + Sync {
    /// Starts async execution and collects yielded chunks. Does NOT wait for promise resolution.
    async fn invoke_stream(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Vec<Value>>;
}

pub struct QuickJsInvoker {
    bridge: Arc<Mutex<QuickJSBridge>>,
}

impl QuickJsInvoker {
    pub fn new(bridge: Arc<Mutex<QuickJSBridge>>) -> Self {
        Self { bridge }
    }
}

#[async_trait(?Send)]
impl JsInvoker for QuickJsInvoker {
    async fn invoke_handler(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Value> {
        let js_request = a2a::request_to_js_value(request);
        let mut bridge = self.bridge.lock().await;
        bridge
            .invoke_js_function(scope, "onChatMessage", js_request)
            .await
    }

    async fn invoke_stream(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Vec<Value>> {
        <Self as JsStreamInvoker>::invoke_stream(self, request, scope).await
    }
}

#[async_trait(?Send)]
impl JsStreamInvoker for QuickJsInvoker {
    /// Stream request: type-safe session (setup → invoke → collect). Promise never resolves (CG4).
    async fn invoke_stream(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<Vec<Value>> {
        let js_request = a2a::request_to_js_value(request);
        let mut bridge = self.bridge.lock().await;

        let session = begin_a2a_yield_session(&mut bridge).await?;
        let session = session.invoke(scope, js_request).await?;
        let yielded = session.collect().await?;
        Ok(yielded)
    }
}

#[async_trait(?Send)]
pub trait RequestRouter: Send + Sync {
    /// Route the request. Scope is type-enforced: caller must pass the invocation scope (e.g. from transport).
    async fn route(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
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

#[async_trait(?Send)]
impl RequestRouter for MethodBasedRouter {
    async fn route(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<a2a::A2aOutcome> {
        match request.method {
            a2a::A2aMethod::TasksGet => {
                let req =
                    serde_json::from_value(request.params.clone()).map_err(BamlRtError::Json)?;
                self.task_handler.handle_get(req).await
            }
            a2a::A2aMethod::TasksList => {
                let req =
                    serde_json::from_value(request.params.clone()).map_err(BamlRtError::Json)?;
                self.task_handler.handle_list(req).await
            }
            a2a::A2aMethod::TasksSubscribe => {
                let req =
                    serde_json::from_value(request.params.clone()).map_err(BamlRtError::Json)?;
                self.task_handler
                    .handle_subscribe(req, request.is_stream)
                    .await
            }
            _ => {
                let start = Instant::now();
                let context_id = scope.context_id().clone();
                let route_span = spans::a2a_route(request.method.as_str(), context_id.as_str());
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
                    method: request.method.as_str().to_string(),
                    request_id: request.id.as_ref().and_then(|id| match id {
                        a2a_types::JSONRPCId::String(s) => Some(s.clone()),
                        a2a_types::JSONRPCId::Integer(n) => Some(n.to_string()),
                        a2a_types::JSONRPCId::Null => None,
                    }),
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
                    let js_span = spans::a2a_js_invoke(request.method.as_str(), request.is_stream);
                    let _js_guard = js_span.enter();
                    let mut normalizer = a2a::JsChunkNormalizer::new(scope);
                    if request.is_stream {
                        let chunks = self.js_invoker.invoke_stream(request, scope).await?;
                        let mut normalized_chunks = Vec::with_capacity(chunks.len() + 2);
                        if matches!(request.method, a2a::A2aMethod::MessageSendStream) {
                            normalized_chunks.push(build_event_chunk(
                                "agent_start",
                                &normalizer,
                                None,
                            ));
                        }
                        for chunk in chunks {
                            let normalized = normalizer.normalize_value(chunk)?;
                            self.result_pipeline.store_result(&normalized).await?;
                            if matches!(request.method, a2a::A2aMethod::MessageSendStream)
                                && normalized.get("message").is_some()
                            {
                                let message_id = extract_message_id(&normalized);
                                let is_partial = is_partial_message(&normalized);
                                normalized_chunks.push(build_event_chunk(
                                    "turn_start",
                                    &normalizer,
                                    message_id.as_deref(),
                                ));
                                normalized_chunks.push(build_event_chunk(
                                    "message_start",
                                    &normalizer,
                                    message_id.as_deref(),
                                ));
                                normalized_chunks.push(normalized);
                                if is_partial {
                                    normalized_chunks.push(build_event_chunk(
                                        "message_update",
                                        &normalizer,
                                        message_id.as_deref(),
                                    ));
                                }
                                normalized_chunks.push(build_event_chunk(
                                    "message_end",
                                    &normalizer,
                                    message_id.as_deref(),
                                ));
                                normalized_chunks.push(build_event_chunk(
                                    "turn_end",
                                    &normalizer,
                                    message_id.as_deref(),
                                ));
                            } else {
                                normalized_chunks.push(normalized);
                            }
                        }
                        if matches!(request.method, a2a::A2aMethod::MessageSendStream) {
                            normalized_chunks.push(build_event_chunk(
                                "agent_end",
                                &normalizer,
                                None,
                            ));
                        }
                        Ok(a2a::A2aOutcome::Stream(normalized_chunks))
                    } else {
                        let result = self.js_invoker.invoke_handler(request, scope).await?;
                        let normalized = normalizer.normalize_value(result)?;
                        self.result_pipeline.store_result(&normalized).await?;
                        Ok(a2a::A2aOutcome::Response(normalized))
                    }
                }
                .await;

                let duration = start.elapsed();
                let duration_ms = duration.as_millis() as u64;
                let success = result.is_ok();
                let mode = if request.is_stream {
                    "stream"
                } else {
                    "non_stream"
                };
                let result_str = if success { "success" } else { "error" };
                metrics::record_quickjs_invoke(mode, result_str, duration);
                if let Err(e) = self
                    .effect_emitter
                    .emit(EffectEvent::A2aCompleted {
                        context_id: context_id.clone(),
                        metadata: effect_metadata,
                        duration_ms,
                        success,
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

fn build_event_chunk(
    event_type: &str,
    normalizer: &a2a::JsChunkNormalizer,
    message_id: Option<&str>,
) -> Value {
    let mut event = serde_json::json!({
        "type": event_type,
        "contextId": normalizer.context_id().as_str(),
        "taskId": normalizer.task_id().as_str(),
        "source": "runtime"
    });
    if let Some(message_id) = message_id
        && let Some(map) = event.as_object_mut()
    {
        map.insert(
            "messageId".to_string(),
            Value::String(message_id.to_string()),
        );
    }
    serde_json::json!({ "event": event })
}

fn extract_message_id(chunk: &Value) -> Option<String> {
    chunk
        .get("message")
        .and_then(|message| message.get("messageId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

fn is_partial_message(chunk: &Value) -> bool {
    let Some(message) = chunk.get("message") else {
        return false;
    };
    message.get("delta").is_some()
        || message.get("streaming").is_some()
        || chunk.get("delta").is_some()
        || chunk.get("partial").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a_types::{GetTaskRequest, ListTasksRequest, SubscribeToTaskRequest};
    use async_trait::async_trait;
    use baml_rt_core::effects::EffectBus;
    use serde_json::json;

    struct MockJsInvoker {
        stream_chunks: Vec<Value>,
    }

    #[async_trait(?Send)]
    impl JsInvoker for MockJsInvoker {
        async fn invoke_handler(
            &self,
            _request: &a2a::A2aRequest,
            _scope: &InvocationScope,
        ) -> Result<Value> {
            Ok(Value::Null)
        }
        async fn invoke_stream(
            &self,
            request: &a2a::A2aRequest,
            scope: &InvocationScope,
        ) -> Result<Vec<Value>> {
            <Self as JsStreamInvoker>::invoke_stream(self, request, scope).await
        }
    }

    #[async_trait(?Send)]
    impl JsStreamInvoker for MockJsInvoker {
        async fn invoke_stream(
            &self,
            _request: &a2a::A2aRequest,
            _scope: &InvocationScope,
        ) -> Result<Vec<Value>> {
            Ok(self.stream_chunks.clone())
        }
    }

    struct NoopTaskHandler;

    #[async_trait(?Send)]
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
            _is_stream: bool,
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
        let effect_emitter: Arc<dyn EffectEmitter> = Arc::new(EffectBus::new());
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
        assert!(request.is_stream);

        let scope = InvocationScope::synthetic_message(agent_id.clone());
        let outcome = router.route(&request, &scope).await.unwrap();
        match outcome {
            a2a::A2aOutcome::Stream(chunks) => {
                let content_chunks: Vec<&Value> = chunks
                    .iter()
                    .filter(|chunk| {
                        chunk.get("message").is_some() || chunk.get("statusUpdate").is_some()
                    })
                    .collect();
                assert_eq!(content_chunks.len(), 2);
                assert_eq!(
                    content_chunks[0]
                        .get("message")
                        .and_then(|m| m.get("parts"))
                        .and_then(|p| p.get(0))
                        .and_then(|p| p.get("text"))
                        .and_then(|t| t.as_str()),
                    Some("a")
                );
                assert!(content_chunks[1].get("statusUpdate").is_some());
            }
            a2a::A2aOutcome::Response(_) => panic!("expected Stream outcome"),
        }
    }
}
