//! A2A request handler interface for non-standard transports.

use crate::a2a;
use crate::a2a_types::SendMessageRequest;
use crate::a2a_store::{
    ProvenanceTaskStore, TaskEventRecorder, TaskRepository, TaskStoreBackend, TaskUpdateQueue,
    TaskUpdateEvent,
};
use crate::error_classifier::{A2aErrorClassifier, ErrorClassifier};
use crate::events::{BroadcastEventEmitter, EventEmitter};
use crate::handlers::{DefaultTaskHandler, TaskHandler};
use crate::request_router::{MethodBasedRouter, QuickJsInvoker, RequestRouter};
use crate::result_deduplicator::{DeduplicatingPipeline, HashResultDeduplicator, ResultDeduplicator};
use crate::result_pipeline::{A2aResultPipeline, ResultStoragePipeline};
use crate::response::{JsonRpcResponseFormatter, ResponseFormatter};
use crate::stream_normalizer::{A2aStreamNormalizer, StreamNormalizer};
 
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, QuickJSConfig};
use baml_rt_core::{BamlRtError, Result};
use baml_rt_core::correlation;
use baml_rt_core::context;
use baml_rt_observability::{metrics, spans};
use baml_rt_tools::tools::ToolFunctionMetadata;
use baml_rt_tools::{ToolHandler, ToolName, ToolSession, ToolTypeSpec};
use baml_rt_tools::tools::ToolSessionContext;
use baml_rt_tools::{ToolFailure, ToolSessionError};
use baml_rt_provenance::{InMemoryProvenanceStore, ProvenanceInterceptor, ProvenanceWriter};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use crate::tools::A2aSessionBundle;

/// Top-level agent type that owns runtime, JS bridge, and A2A comms.
#[derive(Clone)]
pub struct A2aAgent {
    agent_id: baml_rt_core::ids::AgentId,
    runtime: Arc<Mutex<BamlRuntimeManager>>,
    bridge: Arc<Mutex<QuickJSBridge>>,
    task_store: Arc<dyn TaskStoreBackend>,
    provenance_writer: Option<Arc<dyn ProvenanceWriter>>,
    response_formatter: Arc<dyn ResponseFormatter>,
    request_router: Arc<dyn RequestRouter>,
    error_classifier: Arc<dyn ErrorClassifier>,
    update_tx: broadcast::Sender<TaskUpdateEvent>,
}

impl A2aAgent {
    /// Create a builder for configuring agent subcomponents.
    /// 
    /// `agent_id` is automatically generated for provenance tracking.
    pub fn builder() -> A2aAgentBuilder {
        A2aAgentBuilder::new()
    }

    /// Get the agent ID (generated during build)
    pub fn agent_id(&self) -> &baml_rt_core::ids::AgentId {
        &self.agent_id
    }

    /// Access the underlying runtime manager.
    pub fn runtime(&self) -> Arc<Mutex<BamlRuntimeManager>> {
        self.runtime.clone()
    }

    /// Access the underlying JS bridge.
    pub fn bridge(&self) -> Arc<Mutex<QuickJSBridge>> {
        self.bridge.clone()
    }

    /// Access the task store for this agent instance.
    pub fn task_store(&self) -> Arc<dyn TaskStoreBackend> {
        self.task_store.clone()
    }

    /// Access the provenance writer, if configured.
    pub fn provenance_writer(&self) -> Option<Arc<dyn ProvenanceWriter>> {
        self.provenance_writer.clone()
    }

    /// Subscribe to task update events for this agent instance.
    pub fn subscribe_task_updates(&self) -> broadcast::Receiver<TaskUpdateEvent> {
        self.update_tx.subscribe()
    }

    /// Evaluate JavaScript in the agent runtime.
    pub async fn evaluate_js(&self, code: &str) -> Result<Value> {
        let mut bridge = self.bridge.lock().await;
        bridge.evaluate(code).await
    }

    /// Register a JavaScript tool and expose it to BAML-native tool calls.
    pub async fn register_js_tool(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        js_function_code: impl AsRef<str>,
    ) -> Result<()> {
        let name = name.into();
        let parsed = ToolName::parse(&name)?;
        {
            let mut bridge = self.bridge.lock().await;
            bridge.register_js_tool(&name, js_function_code).await?;
        }

        let class_name = ToolFunctionMetadata::derive_class_name(parsed.bundle(), parsed.local());
        let metadata = ToolFunctionMetadata {
            name: parsed.clone(),
            class_name,
            description: description.into(),
            open_input_schema: serde_json::json!({}),
            input_schema,
            output_schema: Value::Null,
            open_input_type: ToolTypeSpec {
                name: "()".to_string(),
                ts_decl: None,
            },
            input_type: ToolTypeSpec {
                name: format!("{}Input", parsed.local().as_str()),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: format!("{}Output", parsed.local().as_str()),
                ts_decl: None,
            },
            tags: Vec::new(),
            secret_requirements: Vec::new(),
            origin: baml_rt_tools::ToolOrigin::Guest,
        };

        let handler: Arc<dyn ToolHandler> = Arc::new(JsToolHandler {
            bridge: self.bridge.clone(),
            tool_name: name,
            metadata: metadata.clone(),
        });

        let registry = {
            let runtime = self.runtime.lock().await;
            runtime.tool_registry()
        };
        let mut registry = registry.lock().await;
        registry.register_dynamic(metadata, handler)?;

        Ok(())
    }

    pub async fn register_a2a_session_tool(&self) -> Result<()> {
        let bundle = A2aSessionBundle::new(Arc::new(self.clone()));
        let registry = {
            let runtime = self.runtime.lock().await;
            runtime.tool_registry()
        };
        let mut registry = registry.lock().await;
        registry.register_bundle(bundle)?;
        Ok(())
    }
}

/// Builder for configuring an A2A agent and its subcomponents.
pub struct A2aAgentBuilder {
    runtime: Option<Arc<Mutex<BamlRuntimeManager>>>,
    bridge: Option<Arc<Mutex<QuickJSBridge>>>,
    quickjs_config: QuickJSConfig,
    register_baml_functions: bool,
    init_js: Vec<String>,
    task_store: Option<Arc<dyn TaskStoreBackend>>,
    provenance_writer: Option<Arc<dyn ProvenanceWriter>>,
    agent_id: Option<baml_rt_core::ids::AgentId>,
    register_a2a_session_tool: bool,
}

impl Default for A2aAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl A2aAgentBuilder {
    /// Create a new builder. `agent_id` will be automatically generated during build().
    pub fn new() -> Self {
        Self {
            runtime: None,
            bridge: None,
            quickjs_config: QuickJSConfig::default(),
            register_baml_functions: true,
            init_js: Vec::new(),
            task_store: None,
            provenance_writer: None,
            agent_id: None, // Will be generated in build()
            register_a2a_session_tool: false,
        }
    }

    /// Provide an existing runtime manager.
    pub fn with_runtime_manager(mut self, runtime: BamlRuntimeManager) -> Self {
        self.runtime = Some(Arc::new(Mutex::new(runtime)));
        self
    }

    /// Provide a shared runtime manager.
    pub fn with_runtime_handle(mut self, runtime: Arc<Mutex<BamlRuntimeManager>>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Provide a shared QuickJS bridge (requires a runtime handle too).
    pub fn with_bridge_handle(mut self, bridge: Arc<Mutex<QuickJSBridge>>) -> Self {
        self.bridge = Some(bridge);
        self
    }

    /// Configure QuickJS runtime options used when creating the bridge.
    pub fn with_quickjs_config(mut self, config: QuickJSConfig) -> Self {
        self.quickjs_config = config;
        self
    }

    /// Enable or disable registration of BAML helper functions.
    pub fn with_baml_helpers(mut self, enabled: bool) -> Self {
        self.register_baml_functions = enabled;
        self
    }

    /// Add JavaScript to evaluate after the bridge is created.
    pub fn with_init_js(mut self, code: impl Into<String>) -> Self {
        self.init_js.push(code.into());
        self
    }

    /// Provide a custom task store backend.
    pub fn with_task_store_backend(mut self, task_store: Arc<dyn TaskStoreBackend>) -> Self {
        self.task_store = Some(task_store);
        self
    }

    /// Provide a custom provenance writer.
    pub fn with_provenance_writer(mut self, writer: Arc<dyn ProvenanceWriter>) -> Self {
        self.provenance_writer = Some(writer);
        self
    }

    pub fn with_a2a_session_tool(mut self, enabled: bool) -> Self {
        self.register_a2a_session_tool = enabled;
        self
    }

    /// Build the agent with the configured subcomponents.
    pub async fn build(self) -> Result<A2aAgent> {
        if self.bridge.is_some() && self.runtime.is_none() {
            return Err(BamlRtError::InvalidArgument(
                "A2aAgentBuilder requires a runtime handle when providing a bridge".to_string(),
            ));
        }

        let runtime = match self.runtime {
            Some(runtime) => runtime,
            None => Arc::new(Mutex::new(BamlRuntimeManager::new()?)),
        };

        // Generate agent_id if not provided (REQUIRED for QuickJS bridge)
        use uuid::Uuid;
        let agent_id = self.agent_id.unwrap_or_else(|| {
            baml_rt_core::ids::AgentId::from_uuid(baml_rt_core::ids::UuidId::new(Uuid::new_v4()))
        });

        let bridge = match self.bridge {
            Some(bridge) => bridge,
            None => {
                let bridge =
                    QuickJSBridge::new_with_config(runtime.clone(), agent_id.clone(), self.quickjs_config).await?;
                Arc::new(Mutex::new(bridge))
            }
        };

        if self.register_baml_functions || !self.init_js.is_empty() {
            let mut bridge_guard = bridge.lock().await;
            if self.register_baml_functions {
                bridge_guard.register_baml_functions().await?;
            }
            for code in self.init_js {
                bridge_guard.evaluate(&code).await?;
            }
        }

        let (update_tx, _update_rx) = broadcast::channel(256);

        let (task_store, provenance_writer) = match (self.task_store, self.provenance_writer) {
            (Some(task_store), provenance_writer) => (task_store, provenance_writer),
            (None, None) => {
                let writer: Arc<dyn ProvenanceWriter> =
                    Arc::new(InMemoryProvenanceStore::new());
                let store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::new(Some(writer.clone()), agent_id.clone()));
                (store, Some(writer))
            }
            (None, Some(writer)) => {
                let store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::new(Some(writer.clone()), agent_id.clone()));
                (store, Some(writer))
            }
        };

        let emitter: Arc<dyn EventEmitter> = Arc::new(BroadcastEventEmitter::new(update_tx.clone()));
        let result_pipeline: Arc<dyn ResultStoragePipeline> =
            Arc::new(A2aResultPipeline::new(task_store.clone(), emitter.clone()));
        let deduplicator: Arc<dyn ResultDeduplicator> = Arc::new(HashResultDeduplicator::new());
        let result_pipeline: Arc<dyn ResultStoragePipeline> =
            Arc::new(DeduplicatingPipeline::new(result_pipeline, deduplicator));
        let response_formatter: Arc<dyn ResponseFormatter> = Arc::new(JsonRpcResponseFormatter);
        let stream_normalizer: Arc<dyn StreamNormalizer> = Arc::new(A2aStreamNormalizer);
        let repository: Arc<dyn TaskRepository> = task_store.clone();
        let recorder: Arc<dyn TaskEventRecorder> = task_store.clone();
        let update_queue: Arc<dyn TaskUpdateQueue> = task_store.clone();
        let task_handler: Arc<dyn TaskHandler> = Arc::new(DefaultTaskHandler::new(
            repository,
            recorder,
            update_queue,
            bridge.clone(),
            emitter.clone(),
        ));
        let js_invoker: Arc<dyn crate::request_router::JsInvoker> = Arc::new(QuickJsInvoker::new(
            bridge.clone(),
            stream_normalizer.clone(),
        ));
        let request_router: Arc<dyn RequestRouter> = Arc::new(MethodBasedRouter::new(
            task_handler.clone(),
            js_invoker,
            result_pipeline.clone(),
        ));
        let error_classifier: Arc<dyn ErrorClassifier> = Arc::new(A2aErrorClassifier);

        if let Some(writer) = provenance_writer.clone() {
            let runtime_guard = runtime.lock().await;
            runtime_guard.register_llm_interceptor(ProvenanceInterceptor::new(writer.clone())).await;
            runtime_guard
                .register_tool_interceptor(ProvenanceInterceptor::new(writer))
                .await;
        }
        let agent = A2aAgent {
            agent_id,
            runtime,
            bridge,
            task_store,
            provenance_writer,
            response_formatter,
            request_router,
            error_classifier,
            update_tx,
        };

        if self.register_a2a_session_tool {
            agent.register_a2a_session_tool().await?;
        }

        {
            let runtime_guard = agent.runtime.lock().await;
            runtime_guard.validate_tool_allowlist_registered().await?;
        }

        Ok(agent)
    }
}

// Default removed - agent_id is generated, use A2aAgent::builder() instead

/// Trait for alternative, non-standard A2A transports.
///
/// The transport receives raw JSON and returns JSON-RPC responses.
#[async_trait(?Send)]
pub trait A2aRequestHandler: Send + Sync {
    async fn handle_a2a(&self, request: Value) -> Result<Vec<Value>>;
}

#[async_trait(?Send)]
impl A2aRequestHandler for A2aAgent {
    async fn handle_a2a(&self, request: Value) -> Result<Vec<Value>> {
        let request_id = a2a::extract_jsonrpc_id(&request);
        let parsed_request = match a2a::A2aRequest::from_value(request) {
            Ok(parsed) => parsed,
            Err(err) => {
                let formatter = JsonRpcResponseFormatter;
                return Ok(vec![formatter.format_error(request_id, &err)]);
            }
        };
        use baml_rt_core::ids::CorrelationId;
        let correlation_id = if let Some(raw) = parsed_request.correlation_id() {
            CorrelationId::parse_temporal(&raw).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "Invalid correlation_id '{}': expected corr-<millis>-<counter>",
                    raw
                ))
            })?
        } else {
            correlation::generate_correlation_id()
        };

        let span = if parsed_request.is_stream {
            spans::a2a_stream(parsed_request.method.as_str(), correlation_id.as_str())
        } else {
            spans::a2a_request(parsed_request.method.as_str(), correlation_id.as_str())
        };
        let _guard = span.enter();
        let start = std::time::Instant::now();
        let method = parsed_request.method;
        let is_stream = parsed_request.is_stream;

        let request_context_id =
            parsed_request.context_id.clone().unwrap_or_else(context::generate_context_id);
        let request_message_id = parsed_request.message_id.clone();
        let request_task_id = parsed_request.task_id.clone();
        let agent_id = self.agent_id.clone();
        let outcome = correlation::with_correlation_id(correlation_id, async move {
            let scope = context::RuntimeScope::new(
                request_context_id,
                agent_id,
                request_message_id,
                request_task_id,
            );
            context::with_scope(scope, async move {
                if matches!(
                    parsed_request.method,
                    a2a::A2aMethod::MessageSend | a2a::A2aMethod::MessageSendStream
                ) && let Ok(params) =
                    serde_json::from_value::<SendMessageRequest>(parsed_request.params.clone())
                {
                    self.task_store.insert_message(&params.message).await;
                }
                self.request_router.route(&parsed_request).await
            })
            .await
        })
        .await;

        let duration = start.elapsed();
        match &outcome {
            Ok(a2a::A2aOutcome::Stream(chunks)) => {
                metrics::record_a2a_request(method.as_str(), "success", is_stream, duration);
                metrics::record_a2a_stream_chunks(method.as_str(), chunks.len());
            }
            Ok(_) => metrics::record_a2a_request(method.as_str(), "success", is_stream, duration),
            Err(err) => {
                metrics::record_a2a_request(method.as_str(), "error", is_stream, duration);
                metrics::record_a2a_error(
                    method.as_str(),
                    self.error_classifier.classify(err),
                    is_stream,
                );
            }
        }

        let responses = match outcome {
            Ok(a2a::A2aOutcome::Response(result)) => {
                vec![self.response_formatter.format_success(request_id, result)]
            }
            Ok(a2a::A2aOutcome::Stream(chunks)) => {
                self.response_formatter.format_stream(request_id, chunks)
            }
            Err(err) => vec![self.response_formatter.format_error(request_id, &err)],
        };

        Ok(responses)
    }
}

impl A2aAgent {
    // Result storage is handled by ResultStoragePipeline.
}

struct JsToolHandler {
    bridge: Arc<Mutex<QuickJSBridge>>,
    tool_name: String,
    metadata: ToolFunctionMetadata,
}

#[async_trait]
impl ToolHandler for JsToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    async fn open_session(&self, ctx: ToolSessionContext) -> Result<Box<dyn ToolSession>> {
        Ok(Box::new(JsToolSession {
            ctx,
            bridge: self.bridge.clone(),
            tool_name: self.tool_name.clone(),
            input: None,
            completed: false,
        }))
    }
}

struct JsToolSession {
    ctx: ToolSessionContext,
    bridge: Arc<Mutex<QuickJSBridge>>,
    tool_name: String,
    input: Option<Value>,
    completed: bool,
}

#[async_trait]
impl ToolSession for JsToolSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.input.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "JS tool session already has input",
            )));
        }
        self.input = Some(input);
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<baml_rt_tools::ToolStep, ToolSessionError> {
        if self.completed {
            return Ok(baml_rt_tools::ToolStep::Done { output: None });
        }
        let input = self.input.take().ok_or_else(|| {
            ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "JS tool session {} has no input",
                self.ctx.session_id
            )))
        })?;
        let bridge = self.bridge.clone();
        let tool_name = self.tool_name.clone();
        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                let mut bridge = bridge.lock().await;
                bridge.invoke_js_tool(&tool_name, input).await
            })
        })
        .await
        .map_err(|err| ToolSessionError::Transport(BamlRtError::QuickJsWithSource {
            context: "js tool join error".to_string(),
            source: Box::new(err),
        }))?
        .map_err(ToolSessionError::Transport)?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            self.completed = true;
            return Ok(baml_rt_tools::ToolStep::Error {
                error: ToolFailure::execution_failed(error.to_string()),
            });
        }
        self.completed = true;
        Ok(baml_rt_tools::ToolStep::Done { output: Some(result) })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.completed = true;
        Ok(())
    }

    async fn abort(&mut self, _reason: Option<String>) -> std::result::Result<(), ToolSessionError> {
        self.completed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::A2aAgent;
    use serde_json::json;

    #[tokio::test]
    async fn js_tool_can_be_called_via_baml_tool_registry() {
        let agent = A2aAgent::builder().build().await.expect("agent build");

        agent
            .register_js_tool(
                "js/add",
                "Adds two numbers",
                json!({
                    "type": "object",
                    "properties": {
                        "a": {"type": "number"},
                        "b": {"type": "number"}
                    },
                    "required": ["a", "b"]
                }),
                r#"(args) => ({ sum: args.a + args.b })"#,
            )
            .await
            .expect("register js tool");

        let runtime = agent.runtime();
        let result = {
            let runtime = runtime.lock().await;
            runtime
                .execute_tool("js/add", json!({"a": 2, "b": 3}))
                .await
                .expect("execute tool")
        };

        assert_eq!(result.get("sum").and_then(|v| v.as_i64()), Some(5));
    }
}
