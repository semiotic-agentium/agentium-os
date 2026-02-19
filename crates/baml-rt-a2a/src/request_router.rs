use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Outcome, Result,
    bus::{A2aEffectMetadata, A2aLivenessRole, EffectEmitter, EffectEvent},
    context::InvocationScope,
    ids::AgentId,
};
use baml_rt_observability::{metrics, spans};
use baml_rt_quickjs::{QuickJSBridge, begin_a2a_yield_session};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{a2a, a2a_types, handlers::TaskHandler, result_pipeline::ResultStoragePipeline};

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
    /// Stream invocation: promise never resolves (CG4). Use only for stream requests.
    async fn invoke_stream(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<baml_rt_core::StreamResult>;
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
                let js_request = a2a::request_to_js_value(&request);
                let mut bridge = bridge.lock().await;
                bridge
                    .invoke_js_function(&scope, "onChatMessage", js_request)
                    .await
            })
        })
        .await
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?
    }

    async fn invoke_stream(
        &self,
        request: &a2a::A2aRequest,
        scope: &InvocationScope,
    ) -> Result<baml_rt_core::StreamResult> {
        let request = request.clone();
        let scope = scope.clone();
        let bridge = self.bridge.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                let js_request = a2a::request_to_js_value(&request);
                let mut bridge = bridge.lock().await;
                let session = begin_a2a_yield_session(&mut bridge).await?;
                let session = session.invoke(&scope, js_request).await?;
                session.collect().await
            })
        })
        .await
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?
    }
}

#[async_trait]
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

#[async_trait]
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
                    .handle_subscribe(req, request.invocation)
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
                    let js_span = spans::a2a_js_invoke(request.method.as_str(), request.invocation);
                    let _js_guard = js_span.enter();
                    let mut normalizer = a2a::JsChunkNormalizer::new(scope);
                    if request.is_stream() {
                        let stream_result = self.js_invoker.invoke_stream(request, scope).await?;
                        let mut normalized_chunks = Vec::with_capacity(stream_result.chunks.len());
                        for chunk in stream_result.chunks {
                            let normalized = normalizer.normalize_value(chunk)?;
                            self.result_pipeline.store_result(&normalized).await?;
                            normalized_chunks.push(normalized);
                        }
                        Ok(a2a::A2aOutcome::Stream(baml_rt_core::StreamResult {
                            chunks: normalized_chunks,
                            completion: stream_result.completion,
                        }))
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
    use async_trait::async_trait;
    use baml_rt_core::{
        InvocationKind,
        bus::BusWithEffects,
        stream_completion::{StreamCompletion, StreamResult},
    };
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
        async fn invoke_stream(
            &self,
            _request: &a2a::A2aRequest,
            _scope: &InvocationScope,
        ) -> Result<StreamResult> {
            Ok(StreamResult {
                chunks: self.stream_chunks.clone(),
                completion: StreamCompletion::SemanticFinal,
            })
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
        let outcome = router.route(&request, &scope).await.unwrap();
        match outcome {
            a2a::A2aOutcome::Stream(stream_result) => {
                let chunks = &stream_result.chunks;
                assert_eq!(chunks.len(), 2);
                assert_eq!(
                    chunks[0]
                        .get("message")
                        .and_then(|m| m.get("parts"))
                        .and_then(|p| p.get(0))
                        .and_then(|p| p.get("text"))
                        .and_then(|t| t.as_str()),
                    Some("a")
                );
                assert!(chunks[1].get("statusUpdate").is_some());
            }
            a2a::A2aOutcome::Response(_) => panic!("expected Stream outcome"),
        }
    }
}
