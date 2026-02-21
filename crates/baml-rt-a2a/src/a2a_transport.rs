//! A2A request handler interface for non-standard transports.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, BamlRtError, Result,
    bus::{BusStream, EffectEmitter},
    context::{self, InvocationScope},
    correlation,
    ids::{ExternalId, MessageId},
    stream_completion::StreamCompletion,
};
use baml_rt_observability::{metrics, spans};
use baml_rt_provenance::{
    A2aGraphStore, ProvEvent, ProvenanceContextMessage, ProvenanceContextReader,
    ProvenanceConversationContextItem, ProvenanceInterceptor, ProvenanceWriter,
};
use baml_rt_quickjs::{
    BamlRuntimeManager, QuickJSBridge, QuickJSConfig, baml_execution::ConversationContextProvider,
};
use baml_rt_tools::{
    ToolFailure, ToolHandler, ToolName, ToolSession, ToolSessionError, ToolTypeSpec,
    tools::{ToolFunctionMetadata, ToolSessionContext},
};
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};

use crate::{
    a2a,
    a2a_store::{
        ConversationContextSource, ProvenanceTaskStore, TaskChunkApplier, TaskEventRecorder,
        TaskRepository, TaskStoreBackend, TaskUpdateEvent, TaskUpdateQueue,
    },
    a2a_types::{JSONRPCId, TaskArtifactUpdateEvent, TaskStatusUpdateEvent},
    error_classifier::{A2aErrorClassifier, ErrorClassifier},
    events::{BroadcastEventEmitter, EventEmitter},
    handlers::{DefaultTaskHandler, TaskHandler},
    request_router::{MethodBasedRouter, QuickJsInvoker, RequestRouter},
    response::{JsonRpcResponseFormatter, ResponseFormatter},
    result_deduplicator::{DeduplicatingPipeline, HashResultDeduplicator, ResultDeduplicator},
    result_pipeline::{A2aResultPipeline, ResultStoragePipeline},
};

/// Single concrete backing store for GraphQLite mode.
/// Exposed as both TaskStoreBackend and ProvenanceWriter from the same Arc.
struct GraphqliteRuntimeStore {
    task_store: Arc<crate::graphqlite_task_subgraph_store::GraphqliteTaskSubgraphStore>,
    provenance: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
}

impl GraphqliteRuntimeStore {
    fn new(provenance: Arc<baml_rt_provenance::GraphqliteProvenanceStore>) -> Arc<Self> {
        let graph: Arc<dyn A2aGraphStore> = provenance.clone();
        let context_reader: Arc<dyn ProvenanceContextReader> = provenance.clone();
        Arc::new(Self {
            task_store: Arc::new(
                crate::graphqlite_task_subgraph_store::GraphqliteTaskSubgraphStore::new(
                    graph,
                    context_reader,
                ),
            ),
            provenance,
        })
    }
}

#[async_trait]
impl TaskRepository for GraphqliteRuntimeStore {
    async fn upsert(&self, task: crate::a2a_types::Task) -> Result<Option<crate::a2a_types::Task>> {
        self.task_store.upsert(task).await
    }
    async fn ensure_task_exists(
        &self,
        task_id: &baml_rt_core::ids::TaskId,
        context_id: Option<&baml_rt_core::ids::ContextId>,
    ) -> Result<()> {
        self.task_store
            .ensure_task_exists(task_id, context_id)
            .await
    }
    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<crate::a2a_types::Task> {
        self.task_store.get(id, history_length).await
    }
    async fn list(
        &self,
        request: &crate::a2a_types::ListTasksRequest,
    ) -> crate::a2a_types::ListTasksResponse {
        self.task_store.list(request).await
    }
    async fn cancel(&self, id: &str) -> Option<crate::a2a_types::Task> {
        self.task_store.cancel(id).await
    }
    async fn insert_message(&self, message: &crate::a2a_types::Message) -> Result<()> {
        self.task_store.insert_message(message).await
    }
}

#[async_trait]
impl TaskEventRecorder for GraphqliteRuntimeStore {
    async fn record_status_update(
        &self,
        task_id: Option<baml_rt_core::ids::TaskId>,
        context_id: Option<baml_rt_core::ids::ContextId>,
        status: crate::a2a_types::TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        self.task_store
            .record_status_update(task_id, context_id, status)
            .await
    }
    async fn record_artifact_update(
        &self,
        task_id: Option<baml_rt_core::ids::TaskId>,
        context_id: Option<baml_rt_core::ids::ContextId>,
        artifact: crate::a2a_types::Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        self.task_store
            .record_artifact_update(task_id, context_id, artifact, append, last_chunk)
            .await
    }
}

#[async_trait]
impl TaskUpdateQueue for GraphqliteRuntimeStore {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        self.task_store.drain_updates(task_id).await
    }
}

#[async_trait]
impl TaskChunkApplier for GraphqliteRuntimeStore {
    async fn apply_task_delta(
        &self,
        task: Option<crate::a2a_types::Task>,
        message: Option<crate::a2a_types::Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Result<Vec<TaskUpdateEvent>> {
        self.task_store
            .apply_task_delta(task, message, status_update, artifact_update)
            .await
    }
}

#[async_trait]
impl ConversationContextSource for GraphqliteRuntimeStore {
    async fn conversation_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.task_store
            .conversation_context(context_id, limit)
            .await
    }
}

#[async_trait]
impl ProvenanceContextReader for GraphqliteRuntimeStore {
    async fn context_messages(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> std::result::Result<Vec<ProvenanceContextMessage>, baml_rt_provenance::ProvenanceError>
    {
        self.provenance.context_messages(context_id, limit).await
    }

    async fn conversation_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> std::result::Result<
        Vec<ProvenanceConversationContextItem>,
        baml_rt_provenance::ProvenanceError,
    > {
        self.provenance
            .conversation_context(context_id, limit)
            .await
    }
}

#[async_trait]
impl ProvenanceWriter for GraphqliteRuntimeStore {
    async fn add_event(
        &self,
        event: ProvEvent,
    ) -> std::result::Result<(), baml_rt_provenance::ProvenanceError> {
        self.provenance.add_event(event).await
    }
}

/// Conversation context from the unified task store (single source of truth).
/// No separate provenance read path; store view and provenance write are one concept.
struct TaskStoreConversationContextProvider {
    store: Arc<dyn ConversationContextSource>,
}

impl TaskStoreConversationContextProvider {
    fn new(store: Arc<dyn ConversationContextSource>) -> Self {
        Self { store }
    }
}

fn conversation_content_to_string(v: &Value) -> String {
    serde_json::to_string(v)
        .inspect_err(|e| {
            tracing::warn!(
                error = %e,
                "conversation context content serialization failed, using Debug"
            );
        })
        .unwrap_or_else(|_| v.to_string())
}

#[async_trait]
impl ConversationContextProvider for TaskStoreConversationContextProvider {
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        let context_id = scope.context_id();
        let items = self
            .store
            .conversation_context(context_id, Some(40))
            .await?;
        if items.is_empty() {
            return Ok(None);
        }
        // Include all provenance items (messages, tool_call, tool_result)
        // so that BAML templates receive the full conversation context
        // including tool interactions from prior turns.
        let entries: Vec<Value> = items
            .into_iter()
            .map(|item| {
                let content = match &item.content {
                    Value::String(s) => s.clone(),
                    other => conversation_content_to_string(other),
                };
                serde_json::json!({
                    "role": item.role,
                    "source": item.source,
                    "content": content,
                })
            })
            .collect();
        Ok(Some(Value::Array(entries)))
    }
}

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
    stream_sessions: Arc<Mutex<HashMap<String, LiveStreamSession>>>,
}

#[derive(Clone)]
struct LiveStreamSession {
    input_tx: async_channel::Sender<Value>,
    output_tx: broadcast::Sender<Value>,
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
                name: format!("{name}Input", name = parsed.local().as_str()),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: format!("{name}Output", name = parsed.local().as_str()),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: Vec::new(),
            access: None,
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
        registry.register_dynamic(metadata, handler)?;

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

/// Task store configuration: either provided or default (must pair with GraphQLite writer).
enum TaskStoreConfig {
    Provided(Arc<dyn TaskStoreBackend>),
    Default,
}

/// Provenance writer configuration: either provided or GraphQLite (task + provenance in same DB).
enum ProvenanceWriterConfig {
    Provided(Arc<dyn ProvenanceWriter>),
    /// Task state and provenance in the same GraphQLite DB; build() creates [ProvenanceTaskStore] over [crate::graphqlite_task_subgraph_store::GraphqliteTaskSubgraphStore].
    Graphqlite(Arc<baml_rt_provenance::GraphqliteProvenanceStore>),
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
    /// - `task_store`: `ProvenanceTaskStore` (no provenance writer)
    /// - `provenance_writer`: None
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

    /// Use GraphQLite for task state and provenance (same DB).
    pub fn with_graphqlite_store(
        mut self,
        store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
    ) -> Self {
        self.provenance_writer = ProvenanceWriterConfig::Graphqlite(store);
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

    /// Use GraphQLite for task state and provenance (same DB).
    pub fn with_graphqlite_store(
        mut self,
        store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
    ) -> Self {
        self.provenance_writer = ProvenanceWriterConfig::Graphqlite(store);
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
                    .map_err(|e| BamlRtError::InvalidArgument(format!(
                        "QuickJS bridge creation failed: {error}",
                        error = e
                    )))?;
                tracing::info!("A2aAgentBuilder::build: QuickJS bridge created successfully");
                Arc::new(Mutex::new(bridge))
            }
        };

        // Bridge promise polling requires effect liveness wiring.
        {
            let mut bridge_guard = bridge.lock().await;
            bridge_guard.set_effect_liveness(self.effect_emitter.clone());
        }

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
                        "init_js evaluation timed out after 30 seconds - code may be blocking: {preview}",
                        preview = if code.len() > 100 {
                            format!("{code}...", code = &code[..100])
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
        let (task_store, provenance_writer) = match (self.task_store, self.provenance_writer) {
            (TaskStoreConfig::Provided(task_store), ProvenanceWriterConfig::Provided(writer)) => {
                (task_store, Some(writer))
            }
            (TaskStoreConfig::Provided(task_store), ProvenanceWriterConfig::Default) => {
                // Task store provided but no writer
                (task_store, None)
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Provided(_writer)) => {
                return Err(BamlRtError::InvalidArgument(
                    "Persistent mode requires explicit task store when using a provided provenance writer".to_string(),
                ));
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Default) => {
                return Err(BamlRtError::InvalidArgument(
                    "Persistent mode requires with_graphqlite_store(...) or explicit task store + provenance writer".to_string(),
                ));
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Graphqlite(store)) => {
                // Single underlying concrete type exposed via both traits.
                // Wrap backend with ProvenanceTaskStore so graph-native provenance events
                // are emitted for message/task/status/artifact writes.
                let runtime_store = GraphqliteRuntimeStore::new(store);
                let provenance_writer: Arc<dyn ProvenanceWriter> = runtime_store.clone();
                let task_store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::with_backend(
                        runtime_store,
                        Some(provenance_writer.clone()),
                        agent_id.clone(),
                    ));
                (task_store, Some(provenance_writer))
            }
            (TaskStoreConfig::Provided(_), ProvenanceWriterConfig::Graphqlite(store)) => {
                // Provided task store is overridden by the single GraphQLite runtime store.
                let runtime_store = GraphqliteRuntimeStore::new(store);
                let provenance_writer: Arc<dyn ProvenanceWriter> = runtime_store.clone();
                let task_store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::with_backend(
                        runtime_store,
                        Some(provenance_writer.clone()),
                        agent_id.clone(),
                    ));
                (task_store, Some(provenance_writer))
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
        let js_invoker: Arc<dyn crate::request_router::JsInvoker> =
            Arc::new(QuickJsInvoker::new(bridge.clone()));
        let request_router: Arc<dyn RequestRouter> = Arc::new(MethodBasedRouter::new(
            task_handler.clone(),
            js_invoker,
            result_pipeline.clone(),
            self.effect_emitter,
            agent_id.clone(),
        ));
        let error_classifier: Arc<dyn ErrorClassifier> = Arc::new(A2aErrorClassifier);

        tracing::debug!("A2aAgentBuilder::build: wiring runtime context/interceptors");
        {
            use tokio::time::{Duration, timeout as tokio_timeout};
            let mut runtime_guard = tokio_timeout(Duration::from_secs(10), runtime.lock())
                .await
                .map_err(|_| {
                    BamlRtError::InvalidArgument(
                        "A2aAgentBuilder::build: timed out acquiring runtime lock".to_string(),
                    )
                })?;
            runtime_guard.set_conversation_context_provider(Arc::new(
                TaskStoreConversationContextProvider::new(task_store.clone()),
            ));
            if let Some(writer) = provenance_writer.clone() {
                tracing::debug!("A2aAgentBuilder::build: register_llm_interceptor start");
                runtime_guard
                    .register_llm_interceptor(ProvenanceInterceptor::new(writer.clone()))
                    .await;
                tracing::debug!("A2aAgentBuilder::build: register_llm_interceptor done");
                tracing::debug!("A2aAgentBuilder::build: register_tool_interceptor start");
                runtime_guard
                    .register_tool_interceptor(ProvenanceInterceptor::new(writer))
                    .await;
                tracing::debug!("A2aAgentBuilder::build: register_tool_interceptor done");
            }
        }
        tracing::debug!("A2aAgentBuilder::build: runtime context/interceptors wired");
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
            stream_sessions: Arc::new(Mutex::new(HashMap::new())),
        };

        tracing::debug!("A2aAgentBuilder::build: validate_tool_allowlist_registered start");
        {
            use tokio::time::{Duration, timeout as tokio_timeout};
            let runtime_guard = tokio_timeout(Duration::from_secs(10), agent.runtime.lock())
                .await
                .map_err(|_| {
                    BamlRtError::InvalidArgument(
                        "A2aAgentBuilder::build: timed out acquiring runtime lock for allowlist validation".to_string(),
                    )
                })?;
            tokio_timeout(
                Duration::from_secs(30),
                runtime_guard.validate_tool_allowlist_registered(),
            )
            .await
            .map_err(|_| {
                BamlRtError::InvalidArgument(
                    "A2aAgentBuilder::build: validate_tool_allowlist_registered timed out"
                        .to_string(),
                )
            })??;
        }
        tracing::debug!("A2aAgentBuilder::build: validate_tool_allowlist_registered done");

        Ok(agent)
    }
}

// Default removed - agent_id is generated, use A2aAgent::builder() instead

#[async_trait]
impl A2aRequestHandler for A2aAgent {
    async fn handle_a2a_stream(&self, request: Value) -> Result<BusStream<Value>> {
        if let Ok(parsed) = a2a::A2aRequest::from_value(request.clone())
            && parsed.method() == a2a::A2aMethod::MessageSendStream
            && parsed.is_stream()
        {
            return self.handle_live_message_stream(request, parsed).await;
        }

        let (tx, rx) = async_channel::unbounded();
        let agent = self.clone();
        tokio::spawn(async move {
            let outcome = agent.handle_a2a_inner(request).await;
            match outcome {
                Ok(responses) => {
                    for response in responses {
                        if tx.send(response).await.is_err() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    let _ = tx
                        .send(serde_json::json!({ "error": err.to_string() }))
                        .await;
                }
            }
            tx.close();
        });
        Ok(Box::pin(async_stream::stream! {
            while let Ok(item) = rx.recv().await {
                yield item;
            }
        }))
    }
}

impl A2aAgent {
    async fn handle_live_message_stream(
        &self,
        request: Value,
        parsed: a2a::A2aRequest,
    ) -> Result<BusStream<Value>> {
        let context_id = parsed
            .context_id
            .clone()
            .unwrap_or_else(context::generate_context_id);
        let session_key = context_id.as_str().to_string();

        let mut attach_input: Option<async_channel::Sender<Value>> = None;
        let mut spawn_payload: Option<(
            String,
            Value,
            async_channel::Receiver<Value>,
            broadcast::Sender<Value>,
        )> = None;

        let mut rx = {
            let mut sessions = self.stream_sessions.lock().await;
            if let Some(session) = sessions.get(&session_key) {
                attach_input = Some(session.input_tx.clone());
                session.output_tx.subscribe()
            } else {
                let (input_tx, input_rx) = async_channel::unbounded::<Value>();
                let (output_tx, _) = broadcast::channel::<Value>(1024);
                let subscriber = output_tx.subscribe();
                sessions.insert(
                    session_key.clone(),
                    LiveStreamSession {
                        input_tx,
                        output_tx: output_tx.clone(),
                    },
                );
                spawn_payload = Some((session_key.clone(), request.clone(), input_rx, output_tx));
                subscriber
            }
        };

        if let Some(input_tx) = attach_input {
            input_tx.send(request).await.map_err(|_| {
                BamlRtError::InvalidArgument(
                    "Active stream session closed before input injection".to_string(),
                )
            })?;
        } else if let Some((key, initial_request, input_rx, output_tx)) = spawn_payload {
            let agent = self.clone();
            tokio::spawn(async move {
                agent
                    .run_live_stream_session(key, initial_request, input_rx, output_tx)
                    .await;
            });
        }

        Ok(Box::pin(async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(item) => yield item,
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }))
    }

    async fn run_live_stream_session(
        &self,
        session_key: String,
        initial_request: Value,
        input_rx: async_channel::Receiver<Value>,
        output_tx: broadcast::Sender<Value>,
    ) {
        let mut current = Some(initial_request);
        while let Some(request_value) = current.take() {
            match self.handle_a2a_outcome_inner(request_value.clone()).await {
                Ok((request_id, outcome)) => match outcome {
                    a2a::A2aOutcome::Response(result) => {
                        let _ = output_tx
                            .send(self.response_formatter.format_success(request_id, result));
                        break;
                    }
                    a2a::A2aOutcome::Stream(stream_result) => {
                        let responses = self
                            .response_formatter
                            .format_stream(request_id, &stream_result);
                        for response in responses {
                            let _ = output_tx.send(response);
                        }
                        // Invariant: continuation is driven only by explicit StreamCompletion,
                        // never inferred from chunk shape.
                        match stream_result.completion {
                            StreamCompletion::InputRequired => match input_rx.recv().await {
                                Ok(next_request) => current = Some(next_request),
                                Err(_) => break,
                            },
                            StreamCompletion::SemanticFinal
                            | StreamCompletion::ChannelClosed
                            | StreamCompletion::Timeout => break,
                        }
                    }
                },
                Err(err) => {
                    let formatter = JsonRpcResponseFormatter;
                    let request_id = a2a::extract_jsonrpc_id(&request_value);
                    let _ = output_tx.send(formatter.format_error(request_id, &err));
                    break;
                }
            }
        }

        let mut sessions = self.stream_sessions.lock().await;
        sessions.remove(&session_key);
    }

    async fn handle_a2a_inner(&self, request: Value) -> Result<Vec<Value>> {
        let fallback_request_id = a2a::extract_jsonrpc_id(&request);
        let (request_id, outcome) = match self.handle_a2a_outcome_inner(request).await {
            Ok(res) => res,
            Err(err) => {
                let formatter = JsonRpcResponseFormatter;
                return Ok(vec![formatter.format_error(fallback_request_id, &err)]);
            }
        };
        let responses = match outcome {
            a2a::A2aOutcome::Response(result) => {
                vec![self.response_formatter.format_success(request_id, result)]
            }
            a2a::A2aOutcome::Stream(stream_result) => self
                .response_formatter
                .format_stream(request_id, &stream_result),
        };
        Ok(responses)
    }

    async fn handle_a2a_outcome_inner(
        &self,
        request: Value,
    ) -> Result<(Option<JSONRPCId>, a2a::A2aOutcome)> {
        let request_id = a2a::extract_jsonrpc_id(&request);
        let parsed_request = match a2a::A2aRequest::from_value(request) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Err(err);
            }
        };
        use baml_rt_core::ids::CorrelationId;
        let correlation_id = if let Some(raw) = parsed_request.correlation_id() {
            CorrelationId::parse_temporal(&raw).ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "Invalid correlation_id '{id}': expected corr-<millis>-<counter>",
                    id = raw
                ))
            })?
        } else {
            correlation::generate_correlation_id()
        };

        let request_context_id = parsed_request
            .context_id
            .clone()
            .unwrap_or_else(context::generate_context_id);
        let request_message_id = parsed_request.message_id.clone().unwrap_or_else(|| {
            MessageId::from_external(ExternalId::new(format!(
                "a2a-{method}-{correlation_id}",
                method = parsed_request.method().as_str(),
                correlation_id = correlation_id.as_str()
            )))
        });
        let request_task_id = parsed_request.task_id.clone();
        let agent_id = self.agent_id.clone();
        let scope = if let Some(task_id) = request_task_id.clone() {
            context::RuntimeScope::task_scope(
                request_context_id.clone(),
                agent_id.clone(),
                request_message_id.clone(),
                task_id,
            )
        } else {
            context::RuntimeScope::message_scope(
                request_context_id.clone(),
                agent_id.clone(),
                request_message_id.clone(),
            )
        };

        let span = if parsed_request.is_stream() {
            spans::a2a_stream(
                Some(&scope),
                parsed_request.method().as_str(),
                correlation_id.as_str(),
            )
        } else {
            spans::a2a_request(
                Some(&scope),
                parsed_request.method().as_str(),
                correlation_id.as_str(),
            )
        };
        let _guard = span.enter();
        let start = std::time::Instant::now();
        let method = parsed_request.method();
        let invocation = parsed_request.invocation;

        let outcome = correlation::with_correlation_id(correlation_id, async move {
            let invocation_scope = InvocationScope::new(scope.clone());
            context::with_scope(scope, async move {
                if let a2a::A2aParams::MessageSendStream(params) = &parsed_request.params {
                    // Persist inbound message independently of task-row materialization timing.
                    self.task_store.insert_message(&params.message).await?;
                }
                let route_span = spans::a2a_route(
                    parsed_request.method().as_str(),
                    invocation_scope.context_id().as_str(),
                );
                let _route_guard = route_span.enter();
                self.request_router
                    .route(&parsed_request, &invocation_scope)
                    .await
            })
            .await
        })
        .await;

        let duration = start.elapsed();
        match &outcome {
            Ok(a2a::A2aOutcome::Stream(stream_result)) => {
                metrics::record_a2a_request(method.as_str(), "success", invocation, duration);
                metrics::record_a2a_stream_chunks(method.as_str(), stream_result.chunks.len());
            }
            Ok(_) => metrics::record_a2a_request(method.as_str(), "success", invocation, duration),
            Err(err) => {
                tracing::warn!(error = ?err, "handle_a2a: routing error");
                metrics::record_a2a_request(method.as_str(), "error", invocation, duration);
                metrics::record_a2a_error(
                    method.as_str(),
                    self.error_classifier.classify(err),
                    invocation,
                );
            }
        }

        outcome.map(|result| (request_id, result))
    }
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
                "JS tool session {session_id} has no input",
                session_id = self.ctx.session_id
            )))
        })?;
        let scope = context::current_scope().map(InvocationScope::new).ok();
        let bridge = self.bridge.clone();
        let tool_name = self.tool_name.clone();
        let handle = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                let mut bridge = bridge.lock().await;
                let scope = scope.ok_or_else(|| {
                    BamlRtError::InvalidArgument(
                        "No invocation scope available for JS tool execution".to_string(),
                    )
                })?;
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
    use std::sync::Arc;

    use baml_rt_core::context::InvocationScope;
    use serde_json::json;

    use super::A2aAgent;

    #[tokio::test]
    async fn js_tool_can_be_called_via_baml_tool_registry() {
        let agent = A2aAgent::builder()
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
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
