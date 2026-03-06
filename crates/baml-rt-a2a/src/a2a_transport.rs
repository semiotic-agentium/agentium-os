//! A2A request handler interface for non-standard transports.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, BamlRtError, Result,
    bus::{BusStream, EffectEmitter},
    context::{self, InvocationScope, OutcomeInvocationContext, RequestScope},
    correlation,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId},
    stream_completion::StreamCompletion,
};
use baml_rt_observability::{metrics, spans};
use baml_rt_provenance::{
    A2aGraphStore, ProvEvent, ProvenanceContextMessage, ProvenanceContextReader,
    ProvenanceConversationContextItem, ProvenanceEffectSubscriber, ProvenanceInterceptor,
    ProvenanceWriter,
};
use baml_rt_quickjs::{
    BamlRuntimeManager, BridgeHandle, QuickJSBridge, QuickJSConfig,
    baml_execution::ConversationContextProvider, invoke_tool_handover,
};
use baml_rt_tools::{
    ToolFailure, ToolHandler, ToolName, ToolRegistry, ToolSession, ToolSessionError, ToolTypeSpec,
    tools::{ToolFunctionMetadata, ToolSessionContext},
};
use baml_tools_system::A2aSessionBundle;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, mpsc};
use tracing::Span;

use crate::{
    a2a,
    a2a_store::{
        ConversationContextSource, ProvenanceTaskStore, TaskChunkApplier, TaskEventRecorder,
        TaskRepository, TaskStoreBackend, TaskUpdateEvent, TaskUpdateQueue, message_role_string,
        metadata_string_map, validated_message_content,
    },
    a2a_types::{
        JSONRPCId, Message, ROLE_USER, StreamChunkView, TaskArtifactUpdateEvent,
        TaskStatusUpdateEvent,
    },
    auto_status::AutoWorkingStatusSubscriber,
    error_classifier::{A2aErrorClassifier, ErrorClassifier},
    events::{BroadcastEventEmitter, EventEmitter},
    handlers::{DefaultTaskHandler, TaskHandler},
    live_stream::{
        LiveResponseChunk, LiveResponseSender, LiveStreamSession, LiveStreamSessionKey, TurnInput,
        WorkingChunkPusher,
    },
    live_stream_working_relay::LiveStreamWorkingRelay,
    request_router::{MethodBasedRouter, QuickJsInvoker, RequestRouter},
    response::{JsonRpcResponseFormatter, ResponseFormatter},
    result_deduplicator::{DeduplicatingPipeline, HashResultDeduplicator, ResultDeduplicator},
    result_pipeline::{A2aResultPipeline, ResultStoragePipeline},
};

/// Payload for spawning a new live stream session (key, context_id, turn_rx).
type LiveStreamSpawnPayload = (
    LiveStreamSessionKey,
    baml_rt_core::ids::ContextId,
    async_channel::Receiver<TurnInput>,
);

/// Single concrete backing store for GraphQLite mode.
/// One instance is built from the builder's store Arc and reused as TaskStoreBackend and
/// ProvenanceWriter; create-stream and tasks.subscribe use this same instance (cardinality one).
struct GraphqliteRuntimeStore {
    task_store: Arc<crate::graphqlite_task_subgraph_store::GraphqliteTaskSubgraphStore>,
    provenance: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
    agent_id: baml_rt_core::ids::AgentId,
}

impl GraphqliteRuntimeStore {
    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Single construction point: one GraphqliteTaskSubgraphStore over the same provenance Arc,
    /// so pipeline and handler share the same graph/connection. agent_id is required for
    /// message provenance (a message is always sent to/from an agent).
    fn new(
        provenance: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
        agent_id: baml_rt_core::ids::AgentId,
    ) -> Arc<Self> {
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
            agent_id,
        })
    }

    async fn add_provenance_event_required(&self, event: ProvEvent, context: &str) -> Result<()> {
        self.provenance.add_event(event).await.map_err(|source| {
            BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to record provenance event for {context}"),
                source: Box::new(source),
            }
        })
    }

    async fn emit_message_lifecycle_event(&self, message: &Message, operation: &str) -> Result<()> {
        let context_id = message.context_id.clone().ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "context_id is required for {operation}; refusing implicit generation"
            ))
        })?;
        let role = message_role_string(&message.role);
        let content = validated_message_content(message, operation)?;
        let metadata = message.metadata.as_ref().map(metadata_string_map);
        tracing::debug!(
            context_id = %context_id,
            message_id = %message.message_id.as_message_id(),
            role = %role,
            "emit_message_lifecycle_event: emitting MessageReceived/MessageSent"
        );
        let event = match (role.as_str(), message.task_id.clone()) {
            (ROLE_USER, Some(task_id)) => ProvEvent::message_received_task(
                context_id,
                task_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                Self::now_millis(),
            ),
            (ROLE_USER, None) => ProvEvent::message_received_global(
                context_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                Self::now_millis(),
            ),
            (_, Some(task_id)) => ProvEvent::message_sent_task(
                context_id,
                task_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                Self::now_millis(),
            ),
            (_, None) => ProvEvent::message_sent_global(
                context_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                Self::now_millis(),
            ),
        };
        self.add_provenance_event_required(event, operation).await
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
        self.emit_message_lifecycle_event(message, "graphqlite insert_message")
            .await?;
        self.task_store.insert_message(message).await
    }
}

#[async_trait]
impl TaskEventRecorder for GraphqliteRuntimeStore {
    async fn record_status_update(
        &self,
        task_id: baml_rt_core::ids::TaskId,
        context_id: Option<baml_rt_core::ids::ContextId>,
        status: crate::a2a_types::TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        self.task_store
            .record_status_update(task_id, context_id, status)
            .await
    }
    async fn record_artifact_update(
        &self,
        task_id: baml_rt_core::ids::TaskId,
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
    tool_registry: Arc<ToolRegistry>,
}

impl TaskStoreConversationContextProvider {
    fn new(store: Arc<dyn ConversationContextSource>, tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            store,
            tool_registry,
        }
    }
}

#[async_trait]
impl ConversationContextProvider for TaskStoreConversationContextProvider {
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        let context_id = scope.context_id();
        // Configuration must come from somewhere else
        // for now lets create a default config
        let config = crate::projection::ProjectionConfig::default();
        let items = self
            .store
            .conversation_context(context_id, Some(config.max_items))
            .await?;
        tracing::debug!(
            context_id = %context_id,
            item_count = items.len(),
            "conversation_history_json: store returned items"
        );
        if items.is_empty() {
            return Ok(None);
        }

        let (entries, stats) =
            crate::projection::project(items, &config, &self.tool_registry);
        tracing::debug!(
            candidates = stats.candidates,
            projected = stats.projected,
            projected_chars = stats.projected_chars,
            dropped_budgeted = stats.dropped_budgeted,
            "conversation_history_json: projection stats"
        );

        Ok(Some(Value::Array(entries)))
    }
}

/// Top-level agent type that owns runtime, JS bridge, and A2A comms.
#[derive(Clone)]
pub struct A2aAgent {
    agent_id: baml_rt_core::ids::AgentId,
    runtime: Arc<Mutex<BamlRuntimeManager>>,
    bridge_handle: Arc<BridgeHandle>,
    task_store: Arc<dyn TaskStoreBackend>,
    #[allow(dead_code)] // passed to router at build; clone does not use the field directly
    result_pipeline: Arc<dyn ResultStoragePipeline>,
    /// Inner pipeline (no dedup) used by live stream path so chunk application always persists.
    live_result_pipeline: Arc<dyn ResultStoragePipeline>,
    provenance_writer: Option<Arc<dyn ProvenanceWriter>>,
    response_formatter: Arc<dyn ResponseFormatter>,
    request_router: Arc<dyn RequestRouter>,
    error_classifier: Arc<dyn ErrorClassifier>,
    update_tx: broadcast::Sender<TaskUpdateEvent>,
    stream_sessions: Arc<Mutex<HashMap<LiveStreamSessionKey, LiveStreamSession>>>,
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
        self.bridge_handle.bridge().clone()
    }

    /// Access the bridge handle for handover dispatch.
    pub fn bridge_handle(&self) -> Arc<BridgeHandle> {
        self.bridge_handle.clone()
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
        let mut bridge = self.bridge_handle.bridge().lock().await;
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
            let mut bridge = self.bridge_handle.bridge().lock().await;
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
            handle: self.bridge_handle.clone(),
            tool_name: name,
            metadata: metadata.clone(),
            agent_id: self.agent_id.clone(),
        });

        let registry = {
            let runtime = self.runtime.lock().await;
            runtime.tool_registry()
        };
        registry.register_dynamic(metadata, handler)?;

        Ok(())
    }

    pub async fn register_a2a_session_tool(&self) -> Result<()> {
        let bundle = A2aSessionBundle::new(Arc::new(self.clone()));
        let registry = {
            let runtime = self.runtime.lock().await;
            runtime.tool_registry()
        };
        registry.register_bundle(bundle)?;
        Ok(())
    }

    pub async fn register_a2a_session_tool_with_handler(
        &self,
        handler: Arc<dyn A2aRequestHandler>,
    ) -> Result<()> {
        let bundle = A2aSessionBundle::new(handler);
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
    register_a2a_session_tool: RegistrationMode,
    a2a_session_route_mode: A2aSessionRouteMode,
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
    register_a2a_session_tool: RegistrationMode,
    a2a_session_route_mode: A2aSessionRouteMode,
    effect_emitter: Arc<dyn EffectEmitter>, // REQUIRED - enforced by typestate
}

/// Runtime configuration: either provided or default.
enum RuntimeConfig {
    Provided(Arc<Mutex<BamlRuntimeManager>>),
    Default,
}

/// Bridge configuration: either provided or auto-created.
enum BridgeConfig {
    Provided(Arc<BridgeHandle>),
    AutoCreate,
}

/// Task store configuration: either provided or default (InMemory).
enum TaskStoreConfig {
    Provided(Arc<dyn TaskStoreBackend>),
    Default,
}

/// Provenance writer configuration: either provided, default (InMemory), or GraphQLite (task + provenance in same DB).
enum ProvenanceWriterConfig {
    Provided(Arc<dyn ProvenanceWriter>),
    /// Task state and provenance in the same GraphQLite DB; build() creates [ProvenanceTaskStore] over [crate::graphqlite_unified_store::GraphqliteUnifiedStore].
    Graphqlite(Arc<baml_rt_provenance::GraphqliteProvenanceStore>),
    Default,
}

/// Agent ID configuration: either provided or auto-generated.
enum AgentIdConfig {
    Provided(baml_rt_core::ids::AgentId),
    AutoGenerate,
}

enum A2aSessionRouteMode {
    SelfAgent,
    ExternalRouter(Arc<dyn A2aRequestHandler>),
}

/// Explicit registration toggle for optional helpers/tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationMode {
    Register,
    Skip,
}

impl RegistrationMode {
    pub const fn should_register(self) -> bool {
        matches!(self, Self::Register)
    }
}

impl From<bool> for RegistrationMode {
    fn from(value: bool) -> Self {
        if value { Self::Register } else { Self::Skip }
    }
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
            register_a2a_session_tool: RegistrationMode::Skip,
            a2a_session_route_mode: A2aSessionRouteMode::SelfAgent,
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

    /// Provide a shared bridge handle (overrides auto-creation).
    /// Requires a runtime handle to be provided as well.
    pub fn with_bridge_handle(mut self, handle: Arc<BridgeHandle>) -> Self {
        self.bridge = BridgeConfig::Provided(handle);
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

    pub fn with_a2a_session_tool(mut self, mode: impl Into<RegistrationMode>) -> Self {
        self.register_a2a_session_tool = mode.into();
        self
    }

    /// Provide an external A2A request handler for session routing.
    ///
    /// Automatically sets `register_a2a_session_tool` to `Register` since
    /// providing a router implies intent to use the `system/internal_a2a` tool.
    pub fn with_a2a_session_router(mut self, handler: Arc<dyn A2aRequestHandler>) -> Self {
        self.a2a_session_route_mode = A2aSessionRouteMode::ExternalRouter(handler);
        self.register_a2a_session_tool = RegistrationMode::Register;
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
            a2a_session_route_mode: self.a2a_session_route_mode,
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

    /// Provide a shared bridge handle (overrides auto-creation).
    /// Requires a runtime handle to be provided as well.
    pub fn with_bridge_handle(mut self, handle: Arc<BridgeHandle>) -> Self {
        self.bridge = BridgeConfig::Provided(handle);
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

    pub fn with_a2a_session_tool(mut self, mode: impl Into<RegistrationMode>) -> Self {
        self.register_a2a_session_tool = mode.into();
        self
    }

    /// Provide an external A2A request handler for session routing.
    ///
    /// Automatically sets `register_a2a_session_tool` to `Register` since
    /// providing a router implies intent to use the `system/internal_a2a` tool.
    pub fn with_a2a_session_router(mut self, handler: Arc<dyn A2aRequestHandler>) -> Self {
        self.a2a_session_route_mode = A2aSessionRouteMode::ExternalRouter(handler);
        self.register_a2a_session_tool = RegistrationMode::Register;
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
        let bridge_handle: Arc<BridgeHandle> = match self.bridge {
            BridgeConfig::Provided(handle) => {
                tracing::debug!("A2aAgentBuilder::build: Using provided bridge handle");
                handle
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
                let raw_bridge = Arc::new(Mutex::new(bridge));
                Arc::new(BridgeHandle::new(raw_bridge, &agent_id.to_string()))
            }
        };

        // Bridge promise polling requires effect liveness wiring.
        {
            let mut bridge_guard = bridge_handle.bridge().lock().await;
            bridge_guard.set_effect_liveness(self.effect_emitter.clone());
        }

        // So tool session path (openToolSession/send/next) can emit ToolStarted for WORKING relay.
        {
            let mut runtime_guard = runtime.lock().await;
            runtime_guard.set_effect_emitter(self.effect_emitter.clone());
        }

        if self.register_baml_functions || !self.init_js.is_empty() {
            tracing::debug!(
                "A2aAgentBuilder::build: Registering BAML functions and/or evaluating init_js"
            );
            let mut bridge_guard = bridge_handle.bridge().lock().await;
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

        let (update_tx, _update_rx) = broadcast::channel(256);

        // Resolve task_store and provenance_writer: single construction path per variant so
        // cardinality is one — the same Arc<dyn TaskStoreBackend> is used for repository,
        // live_result_pipeline, and request_router (create-stream writes and tasks.subscribe reads
        // see the same backend instance).
        let (task_store, provenance_writer) = match (self.task_store, self.provenance_writer) {
            (TaskStoreConfig::Provided(task_store), ProvenanceWriterConfig::Provided(writer)) => {
                (task_store, Some(writer))
            }
            (TaskStoreConfig::Provided(task_store), ProvenanceWriterConfig::Default) => {
                (task_store, None)
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Provided(writer)) => {
                let store: Arc<dyn TaskStoreBackend> = Arc::new(ProvenanceTaskStore::new(
                    Some(writer.clone()),
                    agent_id.clone(),
                ));
                (store, Some(writer))
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Default) => {
                let store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::new(None, agent_id.clone()));
                (store, None)
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Graphqlite(store))
            | (TaskStoreConfig::Provided(_), ProvenanceWriterConfig::Graphqlite(store)) => {
                // Single construction: one GraphqliteRuntimeStore from the provided store Arc;
                // same instance used as TaskStoreBackend and ProvenanceWriter for pipeline and handler.
                let runtime_store = GraphqliteRuntimeStore::new(store, agent_id.clone());
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
        let inner_pipeline: Arc<dyn ResultStoragePipeline> =
            Arc::new(A2aResultPipeline::new(task_store.clone(), emitter.clone()));
        let deduplicator: Arc<dyn ResultDeduplicator> = Arc::new(HashResultDeduplicator::new());
        let result_pipeline: Arc<dyn ResultStoragePipeline> = Arc::new(DeduplicatingPipeline::new(
            inner_pipeline.clone(),
            deduplicator,
        ));
        let response_formatter: Arc<dyn ResponseFormatter> = Arc::new(JsonRpcResponseFormatter);
        // Same task_store Arc used for handler repository so subscribe and create-stream share one backend.
        let repository: Arc<dyn TaskRepository> = task_store.clone();
        let recorder: Arc<dyn TaskEventRecorder> = task_store.clone();
        let update_queue: Arc<dyn TaskUpdateQueue> = task_store.clone();
        let task_handler: Arc<dyn TaskHandler> = Arc::new(DefaultTaskHandler::new(
            repository,
            recorder,
            update_queue,
            bridge_handle.bridge().clone(),
            emitter.clone(),
        ));
        let js_invoker: Arc<dyn crate::request_router::JsInvoker> =
            Arc::new(QuickJsInvoker::new(bridge_handle.clone()));
        // Effect subscribers before router: AutoWorkingStatusSubscriber (tasks.subscribe + store); LiveStreamWorkingRelay (HTTP message.sendStream only).
        let effect_emitter = self.effect_emitter.clone();
        effect_emitter
            .subscribe_effect_subscriber(Arc::new(AutoWorkingStatusSubscriber::new(
                task_store.clone(),
                update_tx.clone(),
            )))
            .await;
        let (stream_sessions, relay) = build_stream_sessions_and_relay(response_formatter.clone());
        // Only for HTTP message.sendStream; internal A2A unchanged.
        effect_emitter
            .subscribe_effect_subscriber(relay.clone())
            .await;
        // Provenance: effect bus is the source of truth for LLM/tool completion (including deferred
        // plan failures). Interceptors only see trace-based completion; when execute_tool_from_baml_result
        // fails (e.g. empty steps), handle.complete(Failure) emits via effect bus.
        if let Some(ref writer) = provenance_writer {
            effect_emitter
                .subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(
                    writer.clone(),
                )))
                .await;
        }
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
            let tool_registry = runtime_guard.tool_registry();
            runtime_guard.set_conversation_context_provider(Arc::new(
                TaskStoreConversationContextProvider::new(task_store.clone(), tool_registry),
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
        // live_result_pipeline uses the same task_store as repository (inner_pipeline wraps task_store).
        let agent = A2aAgent {
            agent_id,
            runtime,
            bridge_handle,
            task_store,
            result_pipeline,
            live_result_pipeline: inner_pipeline,
            provenance_writer,
            response_formatter,
            request_router,
            error_classifier,
            update_tx,
            stream_sessions,
        };

        match (&self.a2a_session_route_mode, self.register_a2a_session_tool) {
            (A2aSessionRouteMode::ExternalRouter(_), RegistrationMode::Skip) => {
                tracing::debug!(
                    "A2aAgentBuilder::build: external a2a session router configured but system/internal_a2a not requested by this agent"
                );
            }
            (_, mode) if mode.should_register() => {
                tracing::debug!("A2aAgentBuilder::build: register_a2a_session_tool start");
                match self.a2a_session_route_mode {
                    A2aSessionRouteMode::SelfAgent => {
                        agent.register_a2a_session_tool().await?;
                    }
                    A2aSessionRouteMode::ExternalRouter(handler) => {
                        agent
                            .register_a2a_session_tool_with_handler(handler)
                            .await?;
                    }
                }
                tracing::debug!("A2aAgentBuilder::build: register_a2a_session_tool done");
            }
            _ => {}
        }

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

// Build order: stream_sessions and pusher exist before relay; relay calls pusher directly.
fn build_stream_sessions_and_relay(
    _response_formatter: Arc<dyn ResponseFormatter>,
) -> (
    Arc<Mutex<HashMap<LiveStreamSessionKey, LiveStreamSession>>>,
    Arc<LiveStreamWorkingRelay>,
) {
    let stream_sessions = Arc::new(Mutex::new(HashMap::new()));
    let pusher = Arc::new(WorkingChunkPusher::new(stream_sessions.clone()));
    let relay = Arc::new(LiveStreamWorkingRelay::new(pusher));
    (stream_sessions, relay)
}

/// Scope resolution for outcome handling. No optionality; branching is on the typed invocation context.
///
/// **Provenance invariant (context,task) not conflated:** For LiveSessionFirstTurn, when the
/// message carries a task_id (e.g. delegated worker task from internal_a2a), use it. Otherwise
/// synthesize a deterministic live-session task id derived from (context_id, message_id).
/// This keeps MessageReceived attributed to the receiver's (agent_id, task_id) without collapsing
/// task_id to context_id.
fn synthesized_live_task_id(context_id: &ContextId, message_id: &MessageId) -> TaskId {
    TaskId::from_external(ExternalId::new(format!(
        "live-task:{}:{}",
        context_id.as_str(),
        message_id.as_str()
    )))
}

fn resolve_scope_for_outcome(
    parsed: &a2a::A2aRequest,
    ctx: &OutcomeInvocationContext,
) -> RequestScope {
    match ctx {
        OutcomeInvocationContext::Standalone => parsed.resolved_scope.clone(),
        OutcomeInvocationContext::LiveSessionFirstTurn { context_id } => {
            let task_id = parsed
                .task_id_opt()
                .cloned()
                .unwrap_or_else(|| synthesized_live_task_id(context_id, parsed.message_id()));
            RequestScope::TaskScoped {
                context_id: context_id.clone(),
                message_id: parsed.message_id().clone(),
                task_id,
            }
        }
        OutcomeInvocationContext::LiveSessionResume {
            context_id,
            task_id,
        } => RequestScope::TaskScoped {
            context_id: context_id.clone(),
            message_id: parsed.message_id().clone(),
            task_id: task_id.clone(),
        },
    }
}

// Default removed - agent_id is generated, use A2aAgent::builder() instead
//
// Design: for message.sendStream the live path is used so multi-turn (including resume
// with task_id) attaches to the same session. Do not gate on task_id: resume requests
// must also use the live path or they take the non-live path and never attach, causing
// E2E (resume with task_id) to diverge from non-E2E (no task_id) which always attached.
// Observability spans are created inside the spawned session task.

#[async_trait]
impl A2aRequestHandler for A2aAgent {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let request = request.into_inner();
        if let Ok(parsed) = a2a::A2aRequest::from_value(request.clone())
            && parsed.method() == a2a::A2aMethod::MessageSendStream
            && parsed.is_stream()
        {
            return self.handle_live_message_stream(request, parsed).await;
        }

        let (tx, rx) = async_channel::unbounded();
        let agent = self.clone();
        tokio::spawn(async move {
            let outcome = agent.handle_a2a_inner(request, tx.clone()).await;
            match outcome {
                Ok(responses) => {
                    for response in responses {
                        if tx.send(response).await.is_err() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    // Send can fail when client disconnected; log at debug only.
                    if tx
                        .send(serde_json::json!({ "error": err.to_string() }))
                        .await
                        .is_err()
                    {
                        tracing::debug!("stream send failed (receiver dropped)");
                    }
                }
            }
            tx.close();
        });
        Ok(Box::pin(async_stream::stream! {
            while let Ok(item) = rx.recv().await {
                yield A2aStreamChunk(item);
            }
        }))
    }
}

impl A2aAgent {
    async fn handle_live_message_stream(
        &self,
        request: Value,
        parsed: a2a::A2aRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let context_id = parsed.context_id().clone();
        let requested_session_key =
            LiveStreamSessionKey::from_context_and_task(&context_id, parsed.task_id_opt());
        let context_session_key = LiveStreamSessionKey::from_context_id(&context_id);

        let (response_tx, response_rx) = async_channel::unbounded::<LiveResponseChunk>();

        let mut spawn_payload: Option<LiveStreamSpawnPayload> = None;

        // Take only the sender under the lock; send after releasing the lock so the session task
        // can run (and take the lock if needed) without deadlocking the handler.
        let turn_tx_to_send = {
            let mut sessions = self.stream_sessions.lock().await;
            if let Some(session) = sessions.get(&requested_session_key) {
                Some(session.turn_tx.clone())
            } else if parsed.task_id_opt().is_some()
                && let Some(session) = sessions.get(&context_session_key)
            {
                tracing::debug!(
                    context_id = %context_id,
                    requested_key = %requested_session_key,
                    context_key = %context_session_key,
                    "live stream resume matched context-scoped session key"
                );
                Some(session.turn_tx.clone())
            } else {
                let (turn_tx, turn_rx) = async_channel::unbounded::<TurnInput>();
                sessions.insert(
                    requested_session_key.clone(),
                    LiveStreamSession {
                        turn_tx: turn_tx.clone(),
                        relay_tx: None,
                    },
                );
                spawn_payload = Some((requested_session_key.clone(), context_id.clone(), turn_rx));
                Some(turn_tx)
            }
        };

        let turn_input = TurnInput {
            request: request.clone(),
            response_tx: LiveResponseSender::new(response_tx),
        };
        if let Some(tx) = turn_tx_to_send {
            tx.send(turn_input).await.map_err(|_| {
                BamlRtError::InvalidArgument(
                    "Active stream session closed before input injection".to_string(),
                )
            })?;
        }

        if let Some((key, session_context_id, turn_rx)) = spawn_payload {
            let agent = self.clone();
            let span = Span::current().clone();
            tokio::spawn(async move {
                let _guard = span.enter();
                agent
                    .run_live_stream_session(key, session_context_id, turn_rx)
                    .await;
            });
        }

        Ok(Box::pin(async_stream::stream! {
            while let Ok(chunk) = response_rx.recv().await {
                yield A2aStreamChunk(chunk.0);
            }
        }))
    }

    async fn run_live_stream_session(
        &self,
        session_key: LiveStreamSessionKey,
        session_context_id: baml_rt_core::ids::ContextId,
        turn_rx: async_channel::Receiver<TurnInput>,
    ) {
        let mut session_task_id: Option<TaskId> = None;
        /// When we break on InputRequired we keep (rx, resume_tx) only. We drop the turn's response_tx so the client stream ends and can send the next turn.
        type SuspendedState = (
            mpsc::Receiver<(Value, usize, Option<StreamCompletion>)>,
            mpsc::Sender<Value>,
        );
        let mut suspended: Option<SuspendedState> = None;

        while let Ok(turn) = turn_rx.recv().await {
            let request_value = turn.request.clone();
            let response_tx = turn.response_tx;

            let (mut rx, resume_tx_opt, response_tx, request_id, session_task_id_str, from_resume) =
                if let Some((rx, resume_tx)) = suspended.take() {
                    // Resume path: send turn request on resume_tx so collector delivers into same JS run; drain same rx.
                    // Keep resume_tx so when we hit InputRequired again we can store suspended = Some((rx, resume_tx)) for the next turn.
                    let request_id = a2a::extract_jsonrpc_id(&turn.request);
                    if resume_tx.send(turn.request).await.is_err() {
                        tracing::debug!("resume_tx send failed (collector dropped)");
                        break;
                    }
                    tracing::debug!(
                        context_id = %session_context_id,
                        "live stream resume: sent request on resume_tx, re-entering drain"
                    );
                    (
                        rx,
                        Some(resume_tx),
                        response_tx,
                        request_id,
                        session_task_id
                            .as_ref()
                            .map(|t| t.as_str().to_string())
                            .or_else(|| {
                                a2a::A2aRequest::from_value(request_value.clone())
                                    .ok()
                                    .and_then(|p| p.task_id_opt().map(|t| t.as_str().to_string()))
                            }),
                        true,
                    )
                } else {
                    let invocation_ctx = match session_task_id.clone() {
                        None => OutcomeInvocationContext::LiveSessionFirstTurn {
                            context_id: session_context_id.clone(),
                        },
                        Some(task_id) => OutcomeInvocationContext::LiveSessionResume {
                            context_id: session_context_id.clone(),
                            task_id,
                        },
                    };
                    let resolved_session_task_id =
                        a2a::A2aRequest::from_value(request_value.clone())
                            .ok()
                            .and_then(|parsed_request| {
                                resolve_scope_for_outcome(&parsed_request, &invocation_ctx)
                                    .task_id_opt()
                                    .map(|task_id| task_id.as_str().to_string())
                            });

                    // Allow short resume bursts without stalling the turn pump.
                    let (resume_tx, resume_rx) = mpsc::channel(16);
                    let resume_channel = Some((resume_tx, resume_rx));
                    let relay_rx = {
                        let mut sessions = self.stream_sessions.lock().await;
                        let session = match sessions.get_mut(&session_key) {
                            Some(s) => s,
                            None => {
                                tracing::error!(
                                    ?session_key,
                                    "live session missing when creating relay channel"
                                );
                                let formatter = JsonRpcResponseFormatter;
                                let request_id = a2a::extract_jsonrpc_id(&request_value);
                                let formatted = formatter.format_error(
                                    request_id,
                                    &BamlRtError::InvalidArgument(
                                        "live session missing".to_string(),
                                    ),
                                );
                                if response_tx
                                    .send(LiveResponseChunk(formatted))
                                    .await
                                    .is_err()
                                {
                                    tracing::debug!(
                                        "live stream error/synthetic-final send failed (receiver dropped)"
                                    );
                                }
                                break;
                            }
                        };
                        // Allow bursty tool/status relay chunks without backpressuring the live session.
                        let (relay_tx, relay_rx) = mpsc::channel(256);
                        session.relay_tx = Some(relay_tx);
                        Some(relay_rx)
                    };
                    let (request_id, outcome) = match self
                        .handle_a2a_outcome_inner(
                            request_value.clone(),
                            invocation_ctx.clone(),
                            resume_channel,
                            relay_rx,
                        )
                        .await
                    {
                        Ok(x) => x,
                        Err(err) => {
                            let formatter = JsonRpcResponseFormatter;
                            let request_id = a2a::extract_jsonrpc_id(&request_value);
                            let formatted = formatter.format_error(request_id, &err);
                            if response_tx
                                .send(LiveResponseChunk(formatted))
                                .await
                                .is_err()
                            {
                                tracing::debug!("live stream error send failed (receiver dropped)");
                            }
                            {
                                let mut sessions = self.stream_sessions.lock().await;
                                if let Some(session) = sessions.get_mut(&session_key) {
                                    session.relay_tx = None;
                                }
                            }
                            break;
                        }
                    };

                    match outcome {
                        a2a::A2aOutcome::Response(result) => {
                            let formatted =
                                self.response_formatter.format_success(request_id, result);
                            if response_tx
                                .send(LiveResponseChunk(formatted))
                                .await
                                .is_err()
                            {
                                tracing::debug!(
                                    "live stream response send failed (receiver dropped)"
                                );
                            }
                            break;
                        }
                        a2a::A2aOutcome::Stream(handle) => (
                            handle.receiver,
                            handle.resume_tx,
                            response_tx,
                            request_id,
                            resolved_session_task_id,
                            false,
                        ),
                    }
                };

            let drain_span = spans::live_stream_drain(session_context_id.as_str());
            let _drain_guard = drain_span.enter();
            tracing::debug!(
                context_id = %session_context_id,
                from_resume,
                "live stream drain started (store_result will run per chunk)"
            );
            let mut completion = None;
            let mut last_task_id: Option<String> = None;
            let mut resume_chunk_count: u32 = 0;

            loop {
                let outcome_msg = rx.recv().await;

                let Some(outcome_msg) = outcome_msg else {
                    // rx closed without terminal completion (collector dropped before finalizing).
                    // Emit completion chunk for client and store for provenance so task lifecycle completes.
                    let tid = last_task_id
                        .clone()
                        .or(session_task_id_str.clone())
                        .or_else(|| {
                            a2a::A2aRequest::from_value(request_value.clone())
                                .ok()
                                .and_then(|p| p.task_id_opt().map(|t| t.as_str().to_string()))
                        })
                        .unwrap_or_else(|| format!("stream-{}", session_context_id));
                    let completion_chunk = json!({
                        "task": {
                            "id": tid,
                            "contextId": session_context_id.as_str(),
                            "status": { "state": "TASK_STATE_COMPLETED" },
                            "final": true
                        }
                    });
                    if let Err(e) = self
                        .live_result_pipeline
                        .store_result(&completion_chunk)
                        .await
                    {
                        tracing::warn!(
                            error = %e,
                            "live stream: store_result for rx-closed completion failed (provenance may show Running)"
                        );
                    }
                    let formatted = self.response_formatter.format_stream_chunk(
                        request_id.clone(),
                        completion_chunk,
                        0_usize,
                        true,
                    );
                    if response_tx
                        .send(LiveResponseChunk(formatted))
                        .await
                        .is_err()
                    {
                        tracing::debug!(
                            "live stream synthetic-final send failed (receiver dropped)"
                        );
                    }
                    break;
                };
                let (chunk, index, comp) = outcome_msg;

                completion = comp;
                let view = StreamChunkView::new(chunk);
                if !view.is_null() {
                    if let Some(tid) = view.task_id() {
                        last_task_id = Some(tid.as_str().to_string());
                    }
                    let store_span =
                        spans::live_stream_store_result(index, view.has_storable_payload());
                    let _store_guard = store_span.enter();
                    let raw_for_store = if view.raw.get("__toolStreamChunk").is_some() {
                        let mut c = view.raw.clone();
                        c.as_object_mut()
                            .and_then(|o| o.remove("__toolStreamChunk"));
                        c
                    } else {
                        view.raw.clone()
                    };
                    let store_result = self.live_result_pipeline.store_result(&raw_for_store).await;
                    let ok = store_result.is_ok();
                    store_span.record("store_result_ok", ok);
                    if let Err(e) = store_result {
                        tracing::warn!(
                            error = %e,
                            "live stream: store_result failed (task/subscribe may miss task)"
                        );
                    }
                }
                if comp == Some(StreamCompletion::InputRequired) && view.is_null() {
                    // Emit a minimal wire chunk so the client receives TASK_STATE_INPUT_REQUIRED and can show the banner.
                    let tid = last_task_id
                        .clone()
                        .or(session_task_id_str.clone())
                        .unwrap_or_else(|| format!("stream-{}", session_context_id));
                    let input_required_chunk = json!({
                        "task": {
                            "id": tid,
                            "contextId": session_context_id.as_str(),
                            "status": { "state": "TASK_STATE_INPUT_REQUIRED" }
                        },
                        "final": false
                    });
                    let formatted = self.response_formatter.format_stream_chunk(
                        request_id.clone(),
                        input_required_chunk,
                        index,
                        false,
                    );
                    if response_tx
                        .send(LiveResponseChunk(formatted))
                        .await
                        .is_err()
                    {
                        tracing::debug!(
                            "live stream input_required chunk send failed (receiver dropped)"
                        );
                    }
                    break;
                }
                let is_final = comp.is_some_and(StreamCompletion::is_wire_final);
                let mut chunk_for_format = view.raw.clone();
                let is_tool_stream = chunk_for_format
                    .as_object_mut()
                    .and_then(|o| o.remove("__toolStreamChunk"))
                    .and_then(|v| v.as_bool())
                    == Some(true);
                let mut formatted = self.response_formatter.format_stream_chunk(
                    request_id.clone(),
                    chunk_for_format,
                    index,
                    is_final,
                );
                if is_tool_stream
                    && let Some(obj) = formatted.as_object_mut()
                    && let Some(result) = obj.get_mut("result")
                    && let Some(result_obj) = result.as_object_mut()
                {
                    result_obj.insert(
                        crate::live_stream_working_relay::A2A_RESULT_TOOL_STREAM_CHUNK.to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                if response_tx
                    .send(LiveResponseChunk(formatted))
                    .await
                    .is_err()
                {
                    tracing::debug!("live stream chunk send failed (receiver dropped)");
                } else if from_resume {
                    resume_chunk_count += 1;
                    if resume_chunk_count == 1 {
                        tracing::debug!(
                            context_id = %session_context_id,
                            "live stream resume drain: first chunk forwarded to client"
                        );
                    }
                }
                if comp.is_some() {
                    break;
                }
            }

            session_task_id = last_task_id
                .or(session_task_id_str)
                .map(|task_id| TaskId::from_external(ExternalId::new(task_id)));

            match completion {
                Some(StreamCompletion::InputRequired) => {
                    if let Some(resume_tx) = resume_tx_opt {
                        // Store only (rx, resume_tx). Dropping response_tx closes the client stream so collect_stream returns and the client can send the next turn.
                        suspended = Some((rx, resume_tx));
                    }
                    {
                        let mut sessions = self.stream_sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&session_key) {
                            session.relay_tx = None;
                        }
                    }
                    continue;
                }
                Some(
                    StreamCompletion::SemanticFinal
                    | StreamCompletion::ChannelClosed
                    | StreamCompletion::Timeout,
                ) => break,
                None => break,
            }
        }

        let mut sessions = self.stream_sessions.lock().await;
        if let Some(session) = sessions.get_mut(&session_key) {
            session.relay_tx = None;
        }
        sessions.remove(&session_key);
    }

    async fn handle_a2a_inner(
        &self,
        request: Value,
        stream_tx: async_channel::Sender<Value>,
    ) -> Result<Vec<Value>> {
        let fallback_request_id = a2a::extract_jsonrpc_id(&request);
        let (request_id, outcome) = match self
            .handle_a2a_outcome_inner(
                request.clone(),
                OutcomeInvocationContext::Standalone,
                None,
                None,
            )
            .await
        {
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
            a2a::A2aOutcome::Stream(handle) => {
                let mut rx = handle.receiver;
                // Stream in real time: receive → format → send immediately. No buffering.
                while let Some((chunk, index, comp)) = rx.recv().await {
                    let view = StreamChunkView::new(chunk);
                    if comp == Some(StreamCompletion::InputRequired) && view.is_null() {
                        break;
                    }
                    let is_final = comp.is_some_and(StreamCompletion::is_wire_final);
                    let mut chunk_for_format = view.raw.clone();
                    let is_tool_stream = chunk_for_format
                        .as_object_mut()
                        .and_then(|o| o.remove("__toolStreamChunk"))
                        .and_then(|v| v.as_bool())
                        == Some(true);
                    let mut formatted = self.response_formatter.format_stream_chunk(
                        request_id.clone(),
                        chunk_for_format,
                        index,
                        is_final,
                    );
                    if is_tool_stream
                        && let Some(obj) = formatted.as_object_mut()
                        && let Some(result) = obj.get_mut("result")
                        && let Some(result_obj) = result.as_object_mut()
                    {
                        result_obj.insert(
                            crate::live_stream_working_relay::A2A_RESULT_TOOL_STREAM_CHUNK
                                .to_string(),
                            serde_json::Value::Bool(true),
                        );
                    }
                    if stream_tx.send(formatted).await.is_err() {
                        break;
                    }
                    if comp.is_some() {
                        break;
                    }
                }
                vec![]
            }
        };
        Ok(responses)
    }

    async fn handle_a2a_outcome_inner(
        &self,
        request: Value,
        invocation_ctx: OutcomeInvocationContext,
        resume_channel: crate::request_router::ResumeChannel,
        relay_rx: Option<mpsc::Receiver<Value>>,
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

        let resolved_scope = resolve_scope_for_outcome(&parsed_request, &invocation_ctx);
        match &resolved_scope {
            RequestScope::MessageScoped { context_id, .. } => {
                tracing::debug!(%context_id, "resolved_scope = MessageScoped");
            }
            RequestScope::TaskScoped {
                context_id,
                task_id,
                ..
            } => {
                tracing::debug!(%context_id, %task_id, "resolved_scope = TaskScoped (resume)");
            }
        }
        let scope =
            context::RuntimeScope::from_request_scope(&resolved_scope, self.agent_id.clone());

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
                    let canonical_context_id = invocation_scope.context_id().clone();
                    // Provenance invariant: use RECEIVER's (context_id, task_id), never sender's.
                    // Scope is resolved from invocation_ctx; for delegation, message carries
                    // worker's task_id; we override from scope so misattribution is impossible.
                    let receiver_task_id = invocation_scope.task_id_opt().cloned();
                    if let Some(ref task_id) = receiver_task_id {
                        self.task_store
                            .ensure_task_exists(task_id, Some(&canonical_context_id))
                            .await?;
                    }
                    // Persist user message for conversation context (first turn has no task_id →
                    // message_received_global; resume has task_id → message_received_task).
                    // INVARIANT (read-after-write): insert_message completes before route() so
                    // conversation_context(context_id, limit) read in BAML sees this message.
                    //
                    // Use scope's context_id and task_id so MessageReceived lands in receiver's
                    // (agent_id, task_id). INVARIANT: (agent,task) is not shared; never use
                    // message.task_id from the wire—it may be the sender's (coordinator's).
                    self.task_store
                        .insert_message_for_receiver(
                            &params.message,
                            canonical_context_id,
                            receiver_task_id,
                        )
                        .await?;
                }
                let route_span = spans::a2a_route(
                    parsed_request.method().as_str(),
                    invocation_scope.context_id().as_str(),
                );
                let _route_guard = route_span.enter();
                self.request_router
                    .route(&parsed_request, &invocation_scope, resume_channel, relay_rx)
                    .await
            })
            .await
        })
        .await;

        let duration = start.elapsed();
        match &outcome {
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
    handle: Arc<BridgeHandle>,
    tool_name: String,
    metadata: ToolFunctionMetadata,
    agent_id: AgentId,
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
        let message_id =
            MessageId::from_external(ExternalId::new(format!("tool-{}", ctx.session_id)));
        let scope = context::RuntimeScope::message_scope(
            ctx.context_id.clone(),
            self.agent_id.clone(),
            message_id,
        );
        let invocation_scope = InvocationScope::new(scope);
        Ok(Box::new(JsToolSession {
            ctx,
            scope: invocation_scope,
            handle: self.handle.clone(),
            tool_name: self.tool_name.clone(),
            input: None,
            completed: false,
        }))
    }
}

struct JsToolSession {
    ctx: ToolSessionContext,
    scope: InvocationScope,
    handle: Arc<BridgeHandle>,
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
        let result: Value = invoke_tool_handover(
            &self.handle,
            self.scope.clone(),
            self.tool_name.clone(),
            input,
        )
        .await
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

    use baml_rt_core::{
        context::InvocationScope,
        ids::{ContextId, ExternalId, MessageId, TaskId},
    };
    use baml_rt_provenance::ProvenanceContextReader;
    use serde_json::json;

    use super::{A2aAgent, GraphqliteRuntimeStore, TaskRepository, synthesized_live_task_id};
    use crate::a2a_types::{A2aMessageId, Message, MessageRole, Part};

    #[test]
    fn synthesized_live_task_id_is_distinct_from_context_id() {
        let context_id = ContextId::new(1772891621615, 18);
        let message_id = MessageId::from_external(ExternalId::new("ui-msg-1772891621613-2"));
        let task_id = synthesized_live_task_id(&context_id, &message_id);
        assert_ne!(
            task_id.as_str(),
            context_id.as_str(),
            "task_id must not collapse to context_id"
        );
    }

    #[test]
    fn synthesized_live_task_id_is_deterministic_for_scope() {
        let context_id = ContextId::new(1772891621615, 18);
        let message_id = MessageId::from_external(ExternalId::new("ui-msg-1772891621613-2"));
        let left = synthesized_live_task_id(&context_id, &message_id);
        let right = synthesized_live_task_id(&context_id, &message_id);
        assert_eq!(
            left, right,
            "same context/message scope must map to one task identity"
        );
    }

    #[tokio::test]
    async fn js_tool_can_be_called_via_baml_tool_registry() {
        let store = baml_rt_provenance::GraphqliteStoreBuilder::in_memory()
            .build()
            .expect("test store");
        let agent = A2aAgent::builder()
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_graphqlite_store(store)
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

    #[tokio::test]
    async fn graphqlite_runtime_store_insert_message_records_provenance_message_event() {
        let provenance = baml_rt_provenance::GraphqliteStoreBuilder::in_memory()
            .build()
            .expect("build store");
        let agent_id = baml_rt_core::ids::AgentId::from_uuid(
            baml_rt_core::ids::UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
        );
        let runtime_store = GraphqliteRuntimeStore::new(provenance.clone(), agent_id);
        let context_id = ContextId::new(99, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-ctx-99-1"));
        runtime_store
            .ensure_task_exists(&task_id, Some(&context_id))
            .await
            .expect("ensure task exists");

        let message = Message {
            message_id: A2aMessageId::incoming(ExternalId::new("ui-msg-99-1")),
            role: MessageRole::User,
            parts: vec![Part {
                text: Some("hello machine".to_string()),
                ..Default::default()
            }],
            context_id: Some(context_id.clone()),
            task_id: Some(task_id),
            reference_task_ids: Vec::new(),
            extensions: Vec::new(),
            metadata: None,
            extra: Default::default(),
        };
        runtime_store
            .insert_message(&message)
            .await
            .expect("insert message");

        let messages = provenance
            .context_messages(&context_id, None)
            .await
            .expect("context messages");
        assert!(
            messages.iter().any(|m| {
                m.message_id.as_str() == "ui-msg-99-1"
                    && m.role == "ROLE_USER"
                    && m.content.iter().any(|line| line.contains("hello machine"))
            }),
            "expected persisted ROLE_USER message in provenance context history"
        );
    }
}
