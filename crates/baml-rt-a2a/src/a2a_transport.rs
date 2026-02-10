//! A2A request handler interface for non-standard transports.

use crate::a2a;
use crate::a2a_store::{
    ProvenanceTaskStore, TaskEventRecorder, TaskRepository, TaskStoreBackend, TaskUpdateEvent,
    TaskUpdateQueue,
};
use crate::a2a_types::SendMessageRequest;
use crate::error_classifier::{A2aErrorClassifier, ErrorClassifier};
use crate::events::{BroadcastEventEmitter, EventEmitter};
use crate::handlers::{DefaultTaskHandler, TaskHandler};
use crate::request_router::{MethodBasedRouter, QuickJsInvoker, RequestRouter};
use crate::response::{JsonRpcResponseFormatter, ResponseFormatter};
use crate::result_deduplicator::{
    DeduplicatingPipeline, HashResultDeduplicator, ResultDeduplicator,
};
use crate::result_pipeline::{A2aResultPipeline, ResultStoragePipeline};
use crate::stream_normalizer::{A2aStreamNormalizer, StreamNormalizer};

use crate::tools::A2aSessionBundle;
use async_channel::{Receiver as AsyncReceiver, Sender as AsyncSender, TryRecvError};
use async_trait::async_trait;
use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::correlation;
use baml_rt_core::effects::EffectEmitter;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_observability::{metrics, spans};
use baml_rt_provenance::{ProvenanceContextReader, ProvenanceInterceptor, ProvenanceWriter};
use baml_rt_quickjs::baml_execution::ConversationContextProvider;
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, QuickJSConfig};
use baml_rt_tools::tools::ToolFunctionMetadata;
use baml_rt_tools::tools::ToolSessionContext;
use baml_rt_tools::{ToolFailure, ToolSessionError};
use baml_rt_tools::{ToolHandler, ToolName, ToolSession, ToolTypeSpec};
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tracing::Instrument;

type A2aWorkMsg = (
    context::RuntimeScope,
    Value,
    oneshot::Sender<Result<Vec<Value>>>,
);

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
    /// A2A worker queue invariant:
    /// sender-side is async `send().await`; worker side is non-blocking `try_recv()`.
    a2a_work_tx: AsyncSender<A2aWorkMsg>,
    a2a_work_rx: Arc<AsyncReceiver<A2aWorkMsg>>,
}

#[derive(Clone)]
struct A2aProvenanceConversationContextProvider {
    reader: Arc<dyn ProvenanceContextReader>,
    limit: usize,
}

impl A2aProvenanceConversationContextProvider {
    fn new(reader: Arc<dyn ProvenanceContextReader>) -> Self {
        Self { reader, limit: 64 }
    }
}

#[async_trait]
impl ConversationContextProvider for A2aProvenanceConversationContextProvider {
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        let messages = self
            .reader
            .context_messages(scope.context_id(), Some(self.limit))
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;

        if messages.is_empty() {
            return Ok(None);
        }

        let current_message_id = scope.message_id().as_str();
        let history: Vec<Value> = messages
            .into_iter()
            .filter(|message| message.message_id.as_str() != current_message_id)
            .filter_map(|item| {
                let role = normalize_context_role(&item.role)?;
                let content = item
                    .content
                    .iter()
                    .map(|part| part.trim())
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if content.is_empty() {
                    return None;
                }
                Some(serde_json::json!({ "role": role, "content": content }))
            })
            .collect();

        if history.is_empty() {
            return Ok(None);
        }
        Ok(Some(Value::Array(history)))
    }
}

fn normalize_context_role(role: &str) -> Option<&'static str> {
    match role {
        crate::a2a_types::ROLE_USER | "user" => Some("user"),
        crate::a2a_types::ROLE_AGENT | "assistant" => Some("assistant"),
        _ => None,
    }
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

    /// Session handle for tool session ops without holding the runtime lock across awaits.
    /// Use when the A2A session worker runs on the same executor (e.g. inside `local_set.run_until`).
    pub async fn tool_session_handle(&self) -> baml_rt_quickjs::ToolSessionExecutionHandle {
        self.runtime.lock().await.tool_session_handle()
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
        bridge.evaluate(None, code).await
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
            baml_decl: None,
            tags: Vec::new(),
            secret_requirements: Vec::new(),
            access: None,
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
        registry.register_dynamic(metadata, handler)?;

        Ok(())
    }

    /// Registers the A2A session tool. Session dispatcher and runtime worker run via channels (MT-safe).
    pub async fn register_a2a_session_tool(&self) -> Result<()> {
        let bundle = A2aSessionBundle::new(Arc::new(self.clone()))?;
        let registry = {
            let runtime = self.runtime.lock().await;
            runtime.tool_registry()
        };
        registry.register_bundle(bundle)?;
        Ok(())
    }
}

/// Builder for configuring an A2A agent and its subcomponents.
pub struct A2aAgentBuilder {
    runtime: RuntimeConfig,
    bridge: BridgeConfig,
    quickjs_config: QuickJSConfig,
    register_baml_functions: bool,
    init_js: Vec<String>,
    task_store: TaskStoreConfig,
    provenance_writer: ProvenanceWriterConfig,
    agent_id: AgentIdConfig,
    register_a2a_session_tool: bool,
}

pub struct A2aAgentBuilderWithEffectEmitter {
    runtime: RuntimeConfig,
    bridge: BridgeConfig,
    quickjs_config: QuickJSConfig,
    register_baml_functions: bool,
    init_js: Vec<String>,
    task_store: TaskStoreConfig,
    provenance_writer: ProvenanceWriterConfig,
    agent_id: AgentIdConfig,
    register_a2a_session_tool: bool,
    effect_emitter: Arc<dyn EffectEmitter>, // REQUIRED - enforced by typestate
}

/// Runtime configuration: either provided or default.
enum RuntimeConfig {
    Provided(Arc<Mutex<BamlRuntimeManager>>),
    Default,
}

/// Bridge configuration: either provided or auto-created.
enum BridgeConfig {
    Provided(Arc<Mutex<QuickJSBridge>>),
    AutoCreate,
}

/// Task store configuration: either provided or default (InMemory).
enum TaskStoreConfig {
    Provided(Arc<dyn TaskStoreBackend>),
    Default,
}

/// Provenance writer configuration: either provided or default (InMemory).
enum ProvenanceWriterConfig {
    Provided(Arc<dyn ProvenanceWriter>),
    Default,
}

/// Agent ID configuration: either provided or auto-generated.
enum AgentIdConfig {
    Provided(baml_rt_core::ids::AgentId),
    AutoGenerate,
}

impl Default for A2aAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl A2aAgentBuilder {
    /// Create a new builder with all defaults.
    ///
    /// Defaults:
    /// - `runtime`: Creates new `BamlRuntimeManager`
    /// - `bridge`: Auto-created from runtime + agent_id + config
    /// - `quickjs_config`: `QuickJSConfig::default()`
    /// - `register_baml_functions`: `true`
    /// - `init_js`: Empty vec
    /// - `task_store`: `ProvenanceTaskStore` (without implicit provenance backend)
    /// - `provenance_writer`: none (must be provided explicitly)
    /// - `agent_id`: Auto-generated UUID
    ///
    /// **REQUIRED**: Call `with_effect_emitter()` before `build()`.
    pub fn new() -> Self {
        Self {
            runtime: RuntimeConfig::Default,
            bridge: BridgeConfig::AutoCreate,
            quickjs_config: QuickJSConfig::default(),
            register_baml_functions: true,
            init_js: Vec::new(),
            task_store: TaskStoreConfig::Default,
            provenance_writer: ProvenanceWriterConfig::Default,
            agent_id: AgentIdConfig::AutoGenerate,
            register_a2a_session_tool: false,
        }
    }

    /// Provide an existing runtime manager (overrides default).
    pub fn with_runtime_manager(mut self, runtime: BamlRuntimeManager) -> Self {
        self.runtime = RuntimeConfig::Provided(Arc::new(Mutex::new(runtime)));
        self
    }

    /// Provide a shared runtime manager (overrides default).
    pub fn with_runtime_handle(mut self, runtime: Arc<Mutex<BamlRuntimeManager>>) -> Self {
        self.runtime = RuntimeConfig::Provided(runtime);
        self
    }

    /// Provide a shared QuickJS bridge (overrides auto-creation).
    /// Requires a runtime handle to be provided as well.
    pub fn with_bridge_handle(mut self, bridge: Arc<Mutex<QuickJSBridge>>) -> Self {
        self.bridge = BridgeConfig::Provided(bridge);
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

    /// Provide a custom task store backend (overrides default).
    pub fn with_task_store_backend(mut self, task_store: Arc<dyn TaskStoreBackend>) -> Self {
        self.task_store = TaskStoreConfig::Provided(task_store);
        self
    }

    /// Provide a custom provenance writer (overrides default).
    pub fn with_provenance_writer(mut self, writer: Arc<dyn ProvenanceWriter>) -> Self {
        self.provenance_writer = ProvenanceWriterConfig::Provided(writer);
        self
    }

    pub fn with_a2a_session_tool(mut self, enabled: bool) -> Self {
        self.register_a2a_session_tool = enabled;
        self
    }

    /// Provide an agent ID (overrides auto-generation).
    pub fn with_agent_id(mut self, agent_id: baml_rt_core::ids::AgentId) -> Self {
        self.agent_id = AgentIdConfig::Provided(agent_id);
        self
    }

    /// Provide an effect emitter for A2A effect tracking (host-inbound).
    ///
    /// **REQUIRED**: This must be called before `build()`.
    /// Returns a builder in a state that allows `build()` to be called.
    pub fn with_effect_emitter(
        self,
        emitter: Arc<dyn EffectEmitter>,
    ) -> A2aAgentBuilderWithEffectEmitter {
        A2aAgentBuilderWithEffectEmitter {
            runtime: self.runtime,
            bridge: self.bridge,
            quickjs_config: self.quickjs_config,
            register_baml_functions: self.register_baml_functions,
            init_js: self.init_js,
            task_store: self.task_store,
            provenance_writer: self.provenance_writer,
            agent_id: self.agent_id,
            register_a2a_session_tool: self.register_a2a_session_tool,
            effect_emitter: emitter,
        }
    }
}

impl A2aAgentBuilderWithEffectEmitter {
    /// Provide an existing runtime manager (overrides default).
    pub fn with_runtime_manager(mut self, runtime: BamlRuntimeManager) -> Self {
        self.runtime = RuntimeConfig::Provided(Arc::new(Mutex::new(runtime)));
        self
    }

    /// Provide a shared runtime manager (overrides default).
    pub fn with_runtime_handle(mut self, runtime: Arc<Mutex<BamlRuntimeManager>>) -> Self {
        self.runtime = RuntimeConfig::Provided(runtime);
        self
    }

    /// Provide a shared QuickJS bridge (overrides auto-creation).
    /// Requires a runtime handle to be provided as well.
    pub fn with_bridge_handle(mut self, bridge: Arc<Mutex<QuickJSBridge>>) -> Self {
        self.bridge = BridgeConfig::Provided(bridge);
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

    /// Provide a custom task store backend (overrides default).
    pub fn with_task_store_backend(mut self, task_store: Arc<dyn TaskStoreBackend>) -> Self {
        self.task_store = TaskStoreConfig::Provided(task_store);
        self
    }

    /// Provide a custom provenance writer (overrides default).
    pub fn with_provenance_writer(mut self, writer: Arc<dyn ProvenanceWriter>) -> Self {
        self.provenance_writer = ProvenanceWriterConfig::Provided(writer);
        self
    }

    pub fn with_a2a_session_tool(mut self, enabled: bool) -> Self {
        self.register_a2a_session_tool = enabled;
        self
    }

    /// Build the agent with the configured subcomponents.
    ///
    /// This method is only available after `with_effect_emitter()` has been called.
    /// The `effect_emitter` field is guaranteed to be present by the type system.
    /// All other fields use defaults if not explicitly provided.
    pub async fn build(self) -> Result<A2aAgent> {
        tracing::info!("A2aAgentBuilder::build: Starting build");

        // Resolve runtime: provided or default
        tracing::debug!("A2aAgentBuilder::build: Resolving runtime");
        let runtime = match self.runtime {
            RuntimeConfig::Provided(runtime) => {
                tracing::debug!("A2aAgentBuilder::build: Using provided runtime");
                runtime
            }
            RuntimeConfig::Default => {
                tracing::debug!("A2aAgentBuilder::build: Creating default runtime");
                Arc::new(Mutex::new(BamlRuntimeManager::new()?))
            }
        };

        // Resolve agent_id: provided or auto-generated
        tracing::debug!("A2aAgentBuilder::build: Resolving agent_id");
        use uuid::Uuid;
        let agent_id = match self.agent_id {
            AgentIdConfig::Provided(id) => {
                tracing::debug!("A2aAgentBuilder::build: Using provided agent_id");
                id
            }
            AgentIdConfig::AutoGenerate => {
                tracing::debug!("A2aAgentBuilder::build: Auto-generating agent_id");
                baml_rt_core::ids::AgentId::from_uuid(
                    baml_rt_core::ids::UuidId::new(Uuid::new_v4()),
                )
            }
        };

        // Resolve bridge: provided or auto-created
        tracing::debug!("A2aAgentBuilder::build: Resolving bridge");
        let bridge = match self.bridge {
            BridgeConfig::Provided(bridge) => {
                tracing::debug!("A2aAgentBuilder::build: Using provided bridge");
                bridge
            }
            BridgeConfig::AutoCreate => {
                tracing::info!(
                    "A2aAgentBuilder::build: Creating QuickJS bridge (this may take a moment)"
                );
                // Add timeout around bridge creation to detect hangs
                use tokio::time::{Duration, timeout as tokio_timeout};
                let bridge_result = tokio_timeout(
                    Duration::from_secs(20),
                    QuickJSBridge::new_with_config(
                        runtime.clone(),
                        agent_id.clone(),
                        self.quickjs_config,
                    ),
                )
                .await;
                let bridge = bridge_result
                    .map_err(|_| BamlRtError::InvalidArgument(
                        "QuickJS bridge creation timed out after 20 seconds - possible deadlock".to_string()
                    ))?
                    .map_err(|e| BamlRtError::InvalidArgument(
                        format!("QuickJS bridge creation failed: {}", e)
                    ))?;
                tracing::info!("A2aAgentBuilder::build: QuickJS bridge created successfully");
                Arc::new(Mutex::new(bridge))
            }
        };

        if self.register_baml_functions || !self.init_js.is_empty() {
            tracing::debug!(
                "A2aAgentBuilder::build: Registering BAML functions and/or evaluating init_js"
            );
            let mut bridge_guard = bridge.lock().await;
            if self.register_baml_functions {
                tracing::debug!("A2aAgentBuilder::build: Calling register_baml_functions()");
                // INVARIANT L1: Bridge initialization must terminate within bounded time
                // Add timeout to detect hangs in function registration
                use tokio::time::{Duration, timeout as tokio_timeout};
                tokio_timeout(
                    Duration::from_secs(10),
                    bridge_guard.register_baml_functions(),
                )
                .await
                .map_err(|_| {
                    BamlRtError::InvalidArgument(
                        "register_baml_functions() timed out after 10 seconds - possible deadlock"
                            .to_string(),
                    )
                })??;
                tracing::debug!("A2aAgentBuilder::build: register_baml_functions() completed");
            }
            for code in self.init_js {
                tracing::debug!("A2aAgentBuilder::build: Evaluating init_js code");
                // INVARIANT L2: Eval operations must yield control within bounded time
                use tokio::time::{Duration, timeout as tokio_timeout};
                tokio_timeout(
                    Duration::from_secs(30), // Longer timeout for user code
                    bridge_guard.evaluate(None, &code),
                )
                .await
                .map_err(|_| {
                    BamlRtError::InvalidArgument(format!(
                        "init_js evaluation timed out after 30 seconds - code may be blocking: {}",
                        if code.len() > 100 {
                            format!("{}...", &code[..100])
                        } else {
                            code.clone()
                        }
                    ))
                })??;
                tracing::debug!("A2aAgentBuilder::build: init_js code evaluated");
            }
        }
        tracing::debug!("A2aAgentBuilder::build: Bridge initialization complete");
        tracing::debug!("A2aAgentBuilder::build: Bridge initialization complete");

        let (update_tx, _update_rx) = broadcast::channel(256);

        // Resolve task_store and provenance_writer: provided or defaults
        let (task_store, provenance_writer, provenance_reader) =
            match (self.task_store, self.provenance_writer) {
                (
                    TaskStoreConfig::Provided(task_store),
                    ProvenanceWriterConfig::Provided(writer),
                ) => {
                    let reader: Arc<dyn ProvenanceContextReader> = writer.clone();
                    (task_store, Some(writer), Some(reader))
                }
                (TaskStoreConfig::Provided(task_store), ProvenanceWriterConfig::Default) => {
                    // Task store provided but no provenance backend configured.
                    (task_store, None, None)
                }
                (TaskStoreConfig::Default, ProvenanceWriterConfig::Provided(writer)) => {
                    // Writer provided but no task store - create task store with writer
                    let store: Arc<dyn TaskStoreBackend> = Arc::new(ProvenanceTaskStore::new(
                        Some(writer.clone()),
                        agent_id.clone(),
                    ));
                    let reader: Arc<dyn ProvenanceContextReader> = writer.clone();
                    (store, Some(writer), Some(reader))
                }
                (TaskStoreConfig::Default, ProvenanceWriterConfig::Default) => {
                    // Default runtime path: no implicit in-memory provenance backend.
                    let store: Arc<dyn TaskStoreBackend> =
                        Arc::new(ProvenanceTaskStore::new(None, agent_id.clone()));
                    (store, None, None)
                }
            };

        let emitter: Arc<dyn EventEmitter> =
            Arc::new(BroadcastEventEmitter::new(update_tx.clone()));
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
            self.effect_emitter,
            agent_id.clone(),
        ));
        let error_classifier: Arc<dyn ErrorClassifier> = Arc::new(A2aErrorClassifier);

        {
            let mut runtime_guard = runtime.lock().await;
            if let Some(reader) = provenance_reader {
                runtime_guard.set_conversation_context_provider(Arc::new(
                    A2aProvenanceConversationContextProvider::new(reader),
                ));
            }
            if let Some(writer) = provenance_writer.clone() {
                runtime_guard
                    .register_llm_interceptor(ProvenanceInterceptor::new(writer.clone()))
                    .await;
                runtime_guard
                    .register_tool_interceptor(ProvenanceInterceptor::new(writer))
                    .await;
            }
        }
        let (a2a_work_tx, a2a_work_rx) = async_channel::unbounded::<A2aWorkMsg>();
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
            a2a_work_tx,
            a2a_work_rx: Arc::new(a2a_work_rx),
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
/// Handler futures may be !Send (QuickJS bridge); session work runs via [`run_handle_a2a`] so the
/// bridge stays a facade over the runtime's event loop (tokio tasks + message passing, no threads).
#[async_trait(?Send)]
pub trait A2aRequestHandler: Send + Sync {
    async fn handle_a2a(&self, request: Value) -> Result<Vec<Value>>;

    /// Run handle_a2a(request) with scope on the handler's worker (runtime event loop).
    /// Used by the session runtime worker; must post to the bridge, not spawn a thread.
    fn run_handle_a2a(
        &self,
        scope: baml_rt_core::context::RuntimeScope,
        request: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>>> + Send>>;
}

#[async_trait(?Send)]
impl A2aRequestHandler for A2aAgent {
    fn run_handle_a2a(
        &self,
        scope: baml_rt_core::context::RuntimeScope,
        request: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>>> + Send>> {
        let work_tx = self.a2a_work_tx.clone();
        let work_rx = self.a2a_work_rx.clone();
        let bridge = self.bridge.clone();
        let agent = self.clone();
        Box::pin(async move {
            let (result_tx, result_rx) = oneshot::channel::<Result<Vec<Value>>>();
            work_tx
                .send((scope, request, result_tx))
                .await
                .map_err(|_| {
                    BamlRtError::ToolExecution("A2A work channel closed before enqueue".to_string())
                })?;

            {
                let guard = bridge.lock().await;
                let work_rx = work_rx.clone();
                let agent = agent.clone();
                guard.post_to_worker_void(move || match work_rx.try_recv() {
                    Ok((scope, request, result_tx)) => {
                        let span = spans::a2a_worker_drain();
                        let _guard = span.enter();
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(r) => r,
                            Err(e) => {
                                let err = BamlRtError::Initialization(format!(
                                    "A2A worker current_thread runtime: {}",
                                    e
                                ));
                                if result_tx.send(Err(err)).is_err() {
                                    tracing::warn!("A2A worker result channel dropped when sending runtime init error");
                                }
                                return;
                            }
                        };
                        let outcome = rt.block_on(context::with_scope(scope, agent.handle_a2a(request)));
                        if result_tx.send(outcome).is_err() {
                            tracing::warn!(
                                "A2A worker result channel dropped before send; caller likely timed out"
                            );
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        tracing::warn!("A2A worker drain posted but queue was empty");
                    }
                    Err(TryRecvError::Closed) => {
                        tracing::warn!("A2A worker drain posted after queue closed");
                    }
                });
            }

            tokio::time::timeout(std::time::Duration::from_secs(30), result_rx)
                .await
                .map_err(|_| {
                    BamlRtError::ToolExecution(
                        "A2A worker result timed out waiting for QuickJS worker drain".to_string(),
                    )
                })?
                .map_err(|_| {
                    BamlRtError::ToolExecution(
                        "A2A worker dropped result channel before completion".to_string(),
                    )
                })?
        })
    }

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

        // One scope per request (per conversation). Multiple concurrent A2A requests each get
        // their own scope; the handler runs inside with_scope(scope, ...) so routing is to the
        // correct conversation and yielded chunks are attributed to that context.
        let request_context_id = parsed_request
            .context_id
            .clone()
            .unwrap_or_else(context::generate_context_id);
        let request_message_id = parsed_request.message_id.clone();
        let request_task_id = parsed_request.task_id.clone();
        let agent_id = self.agent_id.clone();
        let request_scope = match (request_message_id, request_task_id) {
            (Some(message_id), Some(task_id)) => context::RuntimeScope::task_scope(
                request_context_id,
                agent_id.clone(),
                message_id,
                task_id,
            ),
            (Some(message_id), None) => context::RuntimeScope::message_scope(
                request_context_id,
                agent_id.clone(),
                message_id,
            ),
            (None, Some(task_id)) => {
                let message_id = baml_rt_core::ids::MessageId::from_external(
                    baml_rt_core::ids::ExternalId::new(format!(
                        "a2a-task-msg-{}",
                        request_context_id.as_str()
                    )),
                );
                context::RuntimeScope::task_scope(
                    request_context_id,
                    agent_id.clone(),
                    message_id,
                    task_id,
                )
            }
            (None, None) => {
                let message_id = baml_rt_core::ids::MessageId::from_external(
                    baml_rt_core::ids::ExternalId::new(format!(
                        "a2a-msg-{}",
                        request_context_id.as_str()
                    )),
                );
                context::RuntimeScope::message_scope(
                    request_context_id,
                    agent_id.clone(),
                    message_id,
                )
            }
        };
        let span = if parsed_request.is_stream {
            spans::a2a_stream(
                Some(&request_scope),
                parsed_request.method.as_str(),
                correlation_id.as_str(),
            )
        } else {
            spans::a2a_request(
                Some(&request_scope),
                parsed_request.method.as_str(),
                correlation_id.as_str(),
            )
        };
        let start = std::time::Instant::now();
        let method = parsed_request.method;
        let is_stream = parsed_request.is_stream;
        let outcome = correlation::with_correlation_id(correlation_id, async move {
            let scope = request_scope;
            let invocation_scope = InvocationScope::new(scope.clone());
            context::with_scope(scope, async move {
                if parsed_request.method == a2a::A2aMethod::MessageSendStream
                    && let Ok(params) =
                        serde_json::from_value::<SendMessageRequest>(parsed_request.params.clone())
                {
                    self.task_store.insert_message(&params.message).await;
                }
                self.request_router
                    .route(&parsed_request, &invocation_scope)
                    .await
            })
            .await
        })
        .instrument(span)
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

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        // Registry passes empty object {} for one-shot execute; actual args come via send()
        let _ = open_input;
        let session_scope = InvocationScope::new(ctx.scope.clone());
        Ok(Box::new(JsToolSession {
            ctx,
            bridge: self.bridge.clone(),
            tool_name: self.tool_name.clone(),
            scope: session_scope,
            input: None,
            completed: false,
        }))
    }
}

struct JsToolSession {
    ctx: ToolSessionContext,
    bridge: Arc<Mutex<QuickJSBridge>>,
    tool_name: String,
    scope: InvocationScope,
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
        let scope = self.scope.clone();
        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                let mut bridge = bridge.lock().await;
                bridge
                    .invoke_js_tool_with_scope(&scope, &tool_name, input)
                    .await
            })
        })
        .await
        .map_err(|err| {
            ToolSessionError::Transport(BamlRtError::QuickJsWithSource {
                context: "js tool join error".to_string(),
                source: Box::new(err),
            })
        })?
        .map_err(ToolSessionError::Transport)?;
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            self.completed = true;
            return Ok(baml_rt_tools::ToolStep::Error {
                error: ToolFailure::execution_failed(error.to_string()),
            });
        }
        self.completed = true;
        Ok(baml_rt_tools::ToolStep::Done {
            output: Some(result),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.completed = true;
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.completed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::A2aAgent;
    use baml_rt_core::context::InvocationScope;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn js_tool_can_be_called_via_baml_tool_registry() {
        let agent = A2aAgent::builder()
            .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
            .build()
            .await
            .expect("agent build");

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

        let scope = InvocationScope::synthetic_message(agent.agent_id().clone());
        let runtime = agent.runtime();
        let result = {
            let mgr = runtime.lock().await;
            mgr.execute_tool_with_scope(scope.as_scope(), "js/add", json!({"a": 2, "b": 3}))
                .await
                .expect("execute tool")
        };

        assert_eq!(result.get("sum").and_then(|v| v.as_i64()), Some(5));
    }
}
