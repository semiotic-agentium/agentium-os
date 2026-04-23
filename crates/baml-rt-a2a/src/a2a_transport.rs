//! A2A request handler interface for non-standard transports.

use std::{collections::HashMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_conversation::view::{ProvenanceContextMessage, ProvenanceConversationContextItem};
use baml_rt_core::{
    A2aJsChatHost, A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentDispatchAck,
    AgentDispatchRequest, AgentInstanceId, AgentPackageName, BamlRtError, Citation, Result,
    bus::{BusStream, EffectEmitter},
    context::{self, InvocationScope, OutcomeInvocationContext, RequestScope},
    correlation,
    dispatch::invocation_scope_for_agent_dispatch,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId},
    stream_completion::StreamCompletion,
};
use baml_rt_observability::{metrics, spans};
use baml_rt_provenance::{
    A2aGraphStore, ProvEvent, ProvenanceContextReader, ProvenanceEffectSubscriber,
    ProvenanceInterceptor, ProvenanceWriter,
};
use baml_rt_quickjs::{
    BamlRuntimeManager, BridgeHandle, QuickJSBridge, QuickJSConfig,
    baml_execution::ConversationContextProvider, invoke_optional_js_function_handover,
    invoke_tool_handover,
};
use baml_rt_tools::{
    ToolFailure, ToolHandler, ToolName, ToolRegistry, ToolSession, ToolSessionError, ToolTypeSpec,
    prompt_projection::{PromptProjectionItem, project_prompt_context},
    tools::{ToolFunctionMetadata, ToolSessionContext},
};
use baml_tools_system::A2aSessionBundle;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{Instrument, Span};

use crate::{
    a2a,
    a2a_store::{
        ConversationContextSource, ProvenanceTaskStore, ProvenanceWriterConversationSource,
        TaskChunkApplier, TaskEventRecorder, TaskRepository, TaskStoreBackend, TaskUpdateEvent,
        TaskUpdateQueue, message_role_string, metadata_string_map, validated_message_content,
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

/// Single concrete backing store for SurrealDB mode.
/// One instance is built from the builder's store Arc and reused as TaskStoreBackend and
/// ProvenanceWriter; create-stream and tasks.subscribe use this same instance (cardinality one).
struct SurrealRuntimeStore {
    task_store: Arc<crate::task_subgraph_store::TaskSubgraphStore>,
    provenance: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    agent_id: baml_rt_core::ids::AgentId,
}

impl SurrealRuntimeStore {
    fn now_millis() -> u64 {
        baml_rt_core::now_unix_ms("a2a_transport")
    }

    /// Single construction point: one TaskSubgraphStore over the same provenance Arc,
    /// so pipeline and handler share the same store/connection. agent_id is required for
    /// message provenance (a message is always sent to/from an agent).
    fn new(
        provenance: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        agent_id: baml_rt_core::ids::AgentId,
    ) -> Arc<Self> {
        let graph: Arc<dyn A2aGraphStore> = provenance.clone();
        Arc::new(Self {
            task_store: Arc::new(crate::task_subgraph_store::TaskSubgraphStore::new(graph)),
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
        // Extract typed citations from wire metadata before the lossy string-map conversion
        // drops the array. Citations are model-produced ref-table strings (#N, @N, …).
        let citations: Vec<Citation> = message
            .metadata
            .as_ref()
            .and_then(|m| m.get("citations"))
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(|s| Citation::try_new(s).ok())
                    .collect()
            })
            .unwrap_or_default();
        tracing::debug!(
            context_id = %context_id,
            message_id = %message.message_id.as_message_id(),
            role = %role,
            citation_count = citations.len(),
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
                citations,
            ),
            (_, None) => ProvEvent::message_sent_global(
                context_id,
                message.message_id.as_message_id().clone(),
                role,
                content,
                metadata,
                self.agent_id.clone(),
                Self::now_millis(),
                citations,
            ),
        };
        self.add_provenance_event_required(event, operation).await
    }
}

#[async_trait]
impl TaskRepository for SurrealRuntimeStore {
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
        self.emit_message_lifecycle_event(message, "surreal insert_message")
            .await?;
        self.task_store.insert_message(message).await
    }
}

#[async_trait]
impl TaskEventRecorder for SurrealRuntimeStore {
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
impl TaskUpdateQueue for SurrealRuntimeStore {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        self.task_store.drain_updates(task_id).await
    }
}

#[async_trait]
impl TaskChunkApplier for SurrealRuntimeStore {
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
impl ProvenanceContextReader for SurrealRuntimeStore {
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

    async fn conversation_context_with_task(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
        task_id: Option<&baml_rt_core::ids::TaskId>,
    ) -> std::result::Result<
        Vec<ProvenanceConversationContextItem>,
        baml_rt_provenance::ProvenanceError,
    > {
        self.provenance
            .conversation_context_with_task(context_id, limit, task_id)
            .await
    }
}

#[async_trait]
impl ProvenanceWriter for SurrealRuntimeStore {
    async fn add_event(
        &self,
        event: ProvEvent,
    ) -> std::result::Result<(), baml_rt_provenance::ProvenanceError> {
        self.provenance.add_event(event).await
    }
}

/// Projects [`ProvenanceConversationContextItem`] rows into BAML `conversation_history` JSON.
///
/// This is the **runtime** home for the tag pipeline: `ConversationContextSource::conversation_context`
/// → [`to_projection_item`] ([`baml_rt_conversation::provenance_item_to_projection_item`]) →
/// [`baml_rt_tools::prompt_projection::project_prompt_context`]. Episode assembly and
/// `assemble_session_history` in `baml-rt-conversation` use the same `project_*` and
/// [`baml_rt_tools::prompt_projection::ProjectedLineRole`] row rules; they are not a parallel
/// codepath.
///
/// The `source` is always [`ProvenanceWriterConversationSource`] (graph-backed).
struct ProjectingConversationContextProvider {
    source: Arc<dyn ConversationContextSource>,
    tool_registry: Arc<ToolRegistry>,
    /// Archive tables for re-deriving cat-n Read output in history.
    archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
}

impl ProjectingConversationContextProvider {
    fn new(
        source: Arc<dyn ConversationContextSource>,
        tool_registry: Arc<ToolRegistry>,
        archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
    ) -> Self {
        Self {
            source,
            tool_registry,
            archive_ref_tables,
        }
    }
}

#[async_trait]
impl ConversationContextProvider for ProjectingConversationContextProvider {
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        let context_id = scope.context_id();
        let items = self
            .source
            .conversation_context(context_id, Some(40))
            .await?;
        tracing::debug!(
            context_id = %context_id,
            item_count = items.len(),
            "conversation_history_json: context source returned items"
        );
        if items.is_empty() {
            return Ok(None);
        }

        let projection_items = items
            .into_iter()
            .filter_map(to_projection_item)
            .collect::<Vec<_>>();
        if projection_items.is_empty() {
            return Ok(None);
        }

        // Build an archive reader closure if tables are available.
        // Closure re-derives cat-n output from the archive deterministically.
        let context_id_str = context_id.as_str().to_string();
        let tables = self.archive_ref_tables.clone();
        let reader: Option<Box<dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>>> =
            tables.map(|t| {
                let ctx = context_id_str.clone();
                let boxed: Box<dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>> =
                    Box::new(move |archive_ref_str, grep_str, offset, limit| {
                        let short_ref =
                            baml_rt_tools::archive_read::ShortRef::parse(archive_ref_str)?;
                        let ref_table = baml_rt_tools::archive_refs::get_ref_table(&t, &ctx)?;
                        let entry = ref_table.get(short_ref)?;
                        let grep = grep_str
                            .filter(|s| !s.is_empty())
                            .and_then(|s| baml_rt_tools::archive_read::GrepPattern::parse(s).ok());
                        let page = baml_rt_tools::archive_read::grep_paginate(
                            &entry.content,
                            grep.as_ref(),
                            baml_rt_tools::archive_read::LineOffset(offset),
                            baml_rt_tools::archive_read::PageLimit::new(limit),
                        );
                        let formatted = baml_rt_tools::archive_read::format_cat_n(&page.lines);
                        // CLI invocation without $ — role attribution is handled by the
                        // separate history entry that carries this as content.
                        let cmd = match grep_str.filter(|s| !s.is_empty()) {
                            Some(pat) => format!("grep -n '{pat}' {archive_ref_str}"),
                            None => format!("cat -n {archive_ref_str}"),
                        };
                        if page.lines.is_empty() {
                            return Some(format!("{cmd}\n# no matches"));
                        }
                        let range_comment = page.session_range_comment();
                        Some(format!("{cmd}{range_comment}\n{formatted}"))
                    });
                boxed
            });

        // Get or create the ref table for this context so #N refs can be allocated
        // for messages and tool-call descriptions during projection.
        let ref_table_arc = self
            .archive_ref_tables
            .as_deref()
            .map(|t| baml_rt_tools::archive_refs::get_or_create_ref_table(t, &context_id_str))
            .unwrap_or_else(|| std::sync::Arc::new(baml_rt_tools::archive_refs::RefTable::new()));

        Ok(Some(project_prompt_context(
            projection_items,
            self.tool_registry.as_ref(),
            &ref_table_arc,
            reader.as_deref(),
        )))
    }
}

/// Convert a provenance conversation item to a projection item.
pub(crate) fn to_projection_item(
    item: ProvenanceConversationContextItem,
) -> Option<PromptProjectionItem> {
    baml_rt_conversation::provenance_item_to_projection_item(item)
}

// ---------------------------------------------------------------------------
// Provenance wiring — atomic subsystem registration
// ---------------------------------------------------------------------------

/// Witness type: proves that all provenance subsystems have been atomically wired
/// onto the runtime and effect bus. Only constructible via [`wire_provenance_subsystems`].
///
/// Holds the provenance writer so the `A2aAgent` can expose it without an `Option`.
///
/// # Subsystems enforced
///
/// | # | Subsystem | Target | Purpose |
/// |---|-----------|--------|---------|
/// | 1 | [`ProvenanceEffectSubscriber`] | effect bus | LLM/tool completion → provenance events with drift scoring |
/// | 2 | [`ProvenanceInterceptor`] (tool pipeline) | interceptor registry | Behavior-only interception hook (no provenance writes) |
/// | 3 | [`ProjectingConversationContextProvider`] | runtime | Prompt projection from provenance conversation graph |
///
/// **Adding a new provenance-dependent subsystem? Add it to [`wire_provenance_subsystems`].**
#[derive(Clone)]
struct ProvenanceWired {
    writer: Arc<dyn ProvenanceWriter>,
}

/// Atomically wire all provenance subsystems onto the runtime and effect bus.
///
/// Returns [`ProvenanceWired`] as proof that all wiring is complete. The `A2aAgent`
/// requires this witness, so the compiler rejects any `build()` path that skips wiring.
///
/// # Why a single function?
///
/// The provenance interceptor was missing from the tool pipeline for months because
/// the wiring was scattered across two imperative blocks inside `build()`. Co-locating
/// all provenance registrations here makes it impossible to forget one.
///
/// # LLM interceptor intentionally omitted
///
/// `ProvenanceInterceptor` implements both `LLMInterceptor` and `ToolInterceptor`.
/// LLM provenance writes flow exclusively through the effect bus path
/// (`ProvenanceEffectSubscriber`). Registering an LLM interceptor for provenance
/// would duplicate `LlmCallStarted`/`LlmCallCompleted` events.
///
/// Tool provenance writes are also effect-bus sourced; the tool interceptor remains
/// registered as a behavior-only hook point for future policy/gating interceptors.
async fn wire_provenance_subsystems(
    writer: Arc<dyn ProvenanceWriter>,
    runtime: &RwLock<BamlRuntimeManager>,
    effect_emitter: &dyn EffectEmitter,
) -> Result<ProvenanceWired> {
    tracing::debug!("wire_provenance_subsystems: begin");

    // Phase 1: Read shared state from the runtime (single lock acquisition).
    let (tool_registry, archive_ref_tables, interceptor_registry) = {
        let guard = runtime.read().await;
        (
            guard.tool_registry(),
            guard.archive_ref_tables(),
            guard.interceptor_registry(),
        )
    };

    // Subsystem 1: ProvenanceEffectSubscriber → effect bus.
    // Source of truth for LLM/tool completion events including drift scoring,
    // plan tracking, citation resolution, and deferred plan failures.
    {
        let mut subscriber = ProvenanceEffectSubscriber::new(writer.clone());
        let registry_for_citations = tool_registry.clone();
        let registry_for_describer = tool_registry.clone();
        subscriber.set_tool_registry(registry_for_citations);
        subscriber.set_archive_ref_tables(archive_ref_tables.clone());
        subscriber.set_action_describer(Arc::new(
            move |tool_name: Option<&str>, content: &serde_json::Value| {
                let s = registry_for_describer.describe_invocation_with_hint(tool_name, content);
                if s.is_empty() { None } else { Some(s) }
            },
        ));
        // Otherwise the first IntentResolved / PlanGenerated blocks on ONNX init before any
        // provenance row hits the store — the UI stays empty for ~tens of seconds.
        subscriber.warm_drift_models().await;
        effect_emitter
            .subscribe_effect_subscriber(Arc::new(subscriber))
            .await;
    }

    // Subsystem 2: ProvenanceInterceptor → tool pipeline of the interceptor registry.
    // IMPORTANT: interception is for influencing runtime behavior, not provenance recording.
    // Provenance writes (LLM + tool) are effect-bus only via `ProvenanceEffectSubscriber`.
    {
        let mut registry = interceptor_registry.lock().await;
        registry.register_tool_interceptor(ProvenanceInterceptor::new(writer.clone()));
    }

    // Subsystem 3: ConversationContextProvider → runtime.
    // Projects the provenance conversation graph into BAML prompt context
    // using the same tool registry and archive ref tables as drift scoring.
    {
        let conversation_source: Arc<dyn ConversationContextSource> =
            Arc::new(ProvenanceWriterConversationSource::new(writer.clone()));
        let mut guard = runtime.write().await;
        guard.set_conversation_context_provider(Arc::new(
            ProjectingConversationContextProvider::new(
                conversation_source,
                tool_registry,
                Some(archive_ref_tables),
            ),
        ));
    }

    tracing::debug!("wire_provenance_subsystems: all subsystems wired");
    Ok(ProvenanceWired { writer })
}

/// Top-level agent type that owns runtime, JS bridge, and A2A comms.
#[derive(Clone)]
pub struct A2aAgent {
    agent_id: baml_rt_core::ids::AgentId,
    /// Package name of the agent hosted by this `A2aAgent`. Emitted as the
    /// `agent_package` attribute on A2A spans and metrics on the serving side so
    /// operators can slice telemetry by deployed package.
    agent_package: AgentPackageName,
    /// Instance id of the agent hosted by this `A2aAgent`. Emitted as the
    /// `agent_instance_id` attribute alongside `agent_package`.
    agent_instance_id: AgentInstanceId,
    runtime: Arc<RwLock<BamlRuntimeManager>>,
    bridge_handle: Arc<BridgeHandle>,
    task_store: Arc<dyn TaskStoreBackend>,
    #[allow(dead_code)] // passed to router at build; clone does not use the field directly
    result_pipeline: Arc<dyn ResultStoragePipeline>,
    /// Inner pipeline (no dedup) used by live stream path so chunk application always persists.
    live_result_pipeline: Arc<dyn ResultStoragePipeline>,
    /// Witness that all provenance subsystems are wired. Never optional — every
    /// agent has a provenance graph. Use `.provenance_writer()` to access the writer.
    provenance: ProvenanceWired,
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
    pub fn runtime(&self) -> Arc<RwLock<BamlRuntimeManager>> {
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

    /// Access the provenance writer. Always present — every agent has a provenance graph.
    pub fn provenance_writer(&self) -> Arc<dyn ProvenanceWriter> {
        self.provenance.writer.clone()
    }

    /// True when this agent still has an active turn for the given scope.
    ///
    /// This checks both live-stream activity and still-open tool sessions so
    /// hosts can defer re-entrant host deliveries until the originating turn
    /// has actually quiesced.
    pub async fn has_in_flight_turn(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
    ) -> bool {
        let requested_session_key =
            LiveStreamSessionKey::from_context_and_task(context_id, task_id);
        let context_session_key = LiveStreamSessionKey::from_context_id(context_id);
        let has_in_flight_live_stream = {
            let sessions = self.stream_sessions.lock().await;
            sessions
                .get(&requested_session_key)
                .map(|session| session.in_flight)
                .unwrap_or(false)
                || task_id.is_some()
                    && sessions
                        .get(&context_session_key)
                        .map(|session| session.in_flight)
                        .unwrap_or(false)
        };
        if has_in_flight_live_stream {
            return true;
        }

        let runtime = self.runtime.read().await;
        runtime
            .open_session_count_for_scope(context_id, task_id)
            .await
            > 0
    }

    /// True when this agent has any active turn across all sessions,
    /// including open host tool sessions.
    ///
    /// Used by the drain mechanism to wait for all in-flight work to complete
    /// before undeploying an agent.
    pub async fn has_any_in_flight(&self) -> bool {
        let stream_in_flight = {
            let sessions = self.stream_sessions.lock().await;
            sessions.values().any(|session| session.in_flight)
        };
        if stream_in_flight {
            return true;
        }
        // Also check for open host tool sessions (e.g. claude, slack).
        let runtime = self.runtime.read().await;
        runtime.has_any_open_tool_sessions()
    }

    /// Subscribe to task update events for this agent instance.
    pub fn subscribe_task_updates(&self) -> broadcast::Receiver<TaskUpdateEvent> {
        self.update_tx.subscribe()
    }

    /// Evaluate synchronous JavaScript in the agent runtime (no invocation scope).
    /// For setup/init code only — must not invoke BAML functions or JS tools.
    pub async fn evaluate_js(&self, code: &str) -> Result<Value> {
        let mut bridge = self.bridge_handle.bridge().lock().await;
        bridge.eval_sync(code).await
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
            secret_requests: Vec::new(),
            event_sources: Vec::new(),
            config: None,
            config_bundle: None,
            origin: baml_rt_tools::ToolOrigin::Guest,
            backend: baml_rt_tools::ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: baml_rt_tools::SessionPolicy::default(),
        };

        let handler: Arc<dyn ToolHandler> = Arc::new(JsToolHandler {
            handle: self.bridge_handle.clone(),
            tool_name: name,
            metadata: metadata.clone(),
            agent_id: self.agent_id.clone(),
        });

        let registry = {
            let runtime = self.runtime.read().await;
            runtime.tool_registry()
        };
        registry.register_dynamic(metadata, handler)?;

        Ok(())
    }

    pub async fn register_a2a_session_tool(&self) -> Result<()> {
        let bundle = A2aSessionBundle::new(Arc::new(self.clone()));
        let registry = {
            let runtime = self.runtime.read().await;
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
            let runtime = self.runtime.read().await;
            runtime.tool_registry()
        };
        registry.register_bundle(bundle)?;
        Ok(())
    }

    /// Deliver a deterministic host dispatch to the agent's optional `onDispatch` handler.
    ///
    /// Uses the bridge handover lane so this future is [`Send`] (required by `async_trait` HTTP
    /// registries); see [`baml_rt_quickjs::invoke_optional_js_function_handover`].
    pub async fn handle_dispatch(&self, request: AgentDispatchRequest) -> Result<AgentDispatchAck> {
        let scope = invocation_scope_for_agent_dispatch(self.agent_id().clone(), &request);
        let js_payload = serde_json::to_value(&request).map_err(BamlRtError::Json)?;

        let result = invoke_optional_js_function_handover(
            self.bridge_handle().as_ref(),
            scope,
            "onDispatch",
            js_payload,
        )
        .await?;
        let Some(value) = result else {
            return Err(BamlRtError::FunctionNotFound("onDispatch".into()));
        };
        serde_json::from_value(value).map_err(BamlRtError::Json)
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
    agent_identity: AgentIdentityConfig,
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
    agent_identity: AgentIdentityConfig,
    register_a2a_session_tool: RegistrationMode,
    a2a_session_route_mode: A2aSessionRouteMode,
    effect_emitter: Arc<dyn EffectEmitter>, // REQUIRED - enforced by typestate
}

/// Runtime configuration: either provided or default.
enum RuntimeConfig {
    Provided(Arc<RwLock<BamlRuntimeManager>>),
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

/// Provenance graph configuration. A writer is **always** mounted at build time.
enum ProvenanceWriterConfig {
    Provided(Arc<dyn ProvenanceWriter>),
    /// Task state and provenance in the same SurrealDB store (ProvenanceTaskStore over SurrealRuntimeStore).
    Surreal(Arc<baml_rt_provenance::SurrealProvenanceStore>),
    /// Opens a dedicated **in-memory** SurrealDB graph (tests and local defaults). Not optional: no agent without a database.
    Default,
}

/// Agent ID configuration: either provided or auto-generated.
enum AgentIdConfig {
    Provided(baml_rt_core::ids::AgentId),
    AutoGenerate,
}

/// Agent identity (package + instance) configuration: either provided or filled with
/// placeholder values suitable for test fixtures. Production hosts always set this via
/// [`A2aAgentBuilder::with_agent_identity`] so A2A spans/metrics carry the deployed
/// agent's package and instance.
enum AgentIdentityConfig {
    Provided {
        agent_package: AgentPackageName,
        agent_instance_id: AgentInstanceId,
    },
    Default,
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
    /// - `task_store` / `provenance_writer`: default pair mounts an **in-memory Surreal** graph
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
            agent_identity: AgentIdentityConfig::Default,
            register_a2a_session_tool: RegistrationMode::Skip,
            a2a_session_route_mode: A2aSessionRouteMode::SelfAgent,
        }
    }

    /// Provide an existing runtime manager (overrides default).
    pub fn with_runtime_manager(mut self, runtime: BamlRuntimeManager) -> Self {
        self.runtime = RuntimeConfig::Provided(Arc::new(RwLock::new(runtime)));
        self
    }

    /// Provide a shared runtime manager (overrides default).
    pub fn with_runtime_handle(mut self, runtime: Arc<RwLock<BamlRuntimeManager>>) -> Self {
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

    /// Use SurrealDB for task state and provenance (same DB).
    pub fn with_surreal_store(
        mut self,
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    ) -> Self {
        self.provenance_writer = ProvenanceWriterConfig::Surreal(store);
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

    /// Provide the agent's package/instance identity for telemetry.
    ///
    /// These values are emitted as `agent_package` and `agent_instance_id` on A2A spans
    /// and metrics on the serving side. Production hosts resolve them from the deployed
    /// agent's route key; tests that do not care about dashboard identity can skip this
    /// call and pick up the placeholder defaults applied during `build()`.
    pub fn with_agent_identity(
        mut self,
        agent_package: AgentPackageName,
        agent_instance_id: AgentInstanceId,
    ) -> Self {
        self.agent_identity = AgentIdentityConfig::Provided {
            agent_package,
            agent_instance_id,
        };
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
            agent_identity: self.agent_identity,
            register_a2a_session_tool: self.register_a2a_session_tool,
            a2a_session_route_mode: self.a2a_session_route_mode,
            effect_emitter: emitter,
        }
    }
}

impl A2aAgentBuilderWithEffectEmitter {
    /// Provide an existing runtime manager (overrides default).
    pub fn with_runtime_manager(mut self, runtime: BamlRuntimeManager) -> Self {
        self.runtime = RuntimeConfig::Provided(Arc::new(RwLock::new(runtime)));
        self
    }

    /// Provide a shared runtime manager (overrides default).
    pub fn with_runtime_handle(mut self, runtime: Arc<RwLock<BamlRuntimeManager>>) -> Self {
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

    /// Use SurrealDB for task state and provenance (same DB).
    pub fn with_surreal_store(
        mut self,
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    ) -> Self {
        self.provenance_writer = ProvenanceWriterConfig::Surreal(store);
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

    /// Provide the agent's package/instance identity for telemetry. See
    /// [`A2aAgentBuilder::with_agent_identity`] for semantics.
    pub fn with_agent_identity(
        mut self,
        agent_package: AgentPackageName,
        agent_instance_id: AgentInstanceId,
    ) -> Self {
        self.agent_identity = AgentIdentityConfig::Provided {
            agent_package,
            agent_instance_id,
        };
        self
    }

    /// Build the agent with the configured subcomponents.
    ///
    /// This method is only available after `with_effect_emitter()` has been called.
    /// The `effect_emitter` field is guaranteed to be present by the type system.
    /// All other fields use defaults if not explicitly provided.
    pub async fn build(self) -> Result<A2aAgent> {
        tracing::debug!("A2aAgentBuilder::build: Starting build");

        // Resolve runtime: provided or default
        tracing::debug!("A2aAgentBuilder::build: Resolving runtime");
        let runtime = match self.runtime {
            RuntimeConfig::Provided(runtime) => {
                tracing::debug!("A2aAgentBuilder::build: Using provided runtime");
                runtime
            }
            RuntimeConfig::Default => {
                tracing::debug!("A2aAgentBuilder::build: Creating default runtime");
                Arc::new(RwLock::new(BamlRuntimeManager::builder().build()?))
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

        // Resolve agent_package / agent_instance_id for telemetry. Production hosts call
        // `with_agent_identity` explicitly; tests fall back to placeholder labels so they
        // still exercise the same emission path.
        let (agent_package, agent_instance_id) = match self.agent_identity {
            AgentIdentityConfig::Provided {
                agent_package,
                agent_instance_id,
            } => (agent_package, agent_instance_id),
            AgentIdentityConfig::Default => (
                AgentPackageName::parse("unknown")
                    .expect("literal 'unknown' is a valid package identifier"),
                AgentInstanceId::default_id(),
            ),
        };

        // Resolve bridge: provided or auto-created
        tracing::debug!("A2aAgentBuilder::build: Resolving bridge");
        let bridge_handle: Arc<BridgeHandle> = match self.bridge {
            BridgeConfig::Provided(handle) => {
                tracing::debug!("A2aAgentBuilder::build: Using provided bridge handle");
                handle
            }
            BridgeConfig::AutoCreate => {
                tracing::debug!(
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
                tracing::debug!("A2aAgentBuilder::build: QuickJS bridge created successfully");
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
            let mut runtime_guard = runtime.write().await;
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
                    bridge_guard.eval_sync(&code),
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
            (TaskStoreConfig::Provided(_), ProvenanceWriterConfig::Default) => {
                return Err(BamlRtError::InvalidArgument(
                    "A2aAgentBuilder: a provenance graph is required. \
                     With a custom task store you must call .with_provenance_writer(...) or .with_surreal_store(...); \
                     you cannot combine with_task_store_backend alone with the default writer."
                        .into(),
                ));
            }
            (TaskStoreConfig::Provided(task_store), ProvenanceWriterConfig::Provided(writer)) => {
                (task_store, Some(writer))
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Provided(writer)) => {
                let store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::new(writer.clone(), agent_id.clone()));
                (store, Some(writer))
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Default) => {
                let prov = baml_rt_provenance::SurrealStoreBuilder::in_memory_isolated()
                    .build()
                    .await
                    .map_err(|e| {
                        BamlRtError::InvalidArgument(format!(
                            "A2aAgentBuilder: failed to open default in-memory provenance store: {e}"
                        ))
                    })?;
                let runtime_store = SurrealRuntimeStore::new(prov, agent_id.clone());
                let w: Arc<dyn ProvenanceWriter> = runtime_store.clone();
                let task_store: Arc<dyn TaskStoreBackend> = Arc::new(
                    ProvenanceTaskStore::with_backend(runtime_store, w.clone(), agent_id.clone()),
                );
                (task_store, Some(w))
            }
            (TaskStoreConfig::Default, ProvenanceWriterConfig::Surreal(store))
            | (TaskStoreConfig::Provided(_), ProvenanceWriterConfig::Surreal(store)) => {
                // Single construction: one SurrealRuntimeStore from the provided store Arc;
                // same instance used as TaskStoreBackend and ProvenanceWriter for pipeline and handler.
                let runtime_store = SurrealRuntimeStore::new(store, agent_id.clone());
                let provenance_writer: Arc<dyn ProvenanceWriter> = runtime_store.clone();
                let task_store: Arc<dyn TaskStoreBackend> =
                    Arc::new(ProvenanceTaskStore::with_backend(
                        runtime_store,
                        provenance_writer.clone(),
                        agent_id.clone(),
                    ));
                (task_store, Some(provenance_writer))
            }
        };

        let writer = provenance_writer.expect("build always mounts a ProvenanceWriter");

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
        // Provenance: all subsystems (effect subscriber, tool interceptor, conversation
        // context) wired atomically. See `wire_provenance_subsystems` doc for the full list.
        let provenance =
            wire_provenance_subsystems(writer, &runtime, effect_emitter.as_ref()).await?;
        let request_router: Arc<dyn RequestRouter> = Arc::new(MethodBasedRouter::new(
            task_handler.clone(),
            js_invoker,
            result_pipeline.clone(),
            self.effect_emitter,
            agent_id.clone(),
        ));
        let error_classifier: Arc<dyn ErrorClassifier> = Arc::new(A2aErrorClassifier);

        // live_result_pipeline uses the same task_store as repository (inner_pipeline wraps task_store).
        let agent = A2aAgent {
            agent_id,
            agent_package,
            agent_instance_id,
            runtime,
            bridge_handle,
            task_store,
            result_pipeline,
            live_result_pipeline: inner_pipeline,
            provenance,
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
            let runtime_guard = tokio_timeout(Duration::from_secs(10), agent.runtime.read())
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

impl A2aJsChatHost for A2aAgent {}

/// Signals whether the outer turn loop should continue or break after an instrumented turn body.
enum TurnAction {
    Continue,
    Break,
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
        let turn_target = {
            let mut sessions = self.stream_sessions.lock().await;
            if let Some(session) = sessions.get_mut(&requested_session_key) {
                if session.in_flight {
                    return Err(BamlRtError::Conflict(format!(
                        "Concurrent message.sendStream rejected for context {context_id} (session key {requested_session_key})"
                    )));
                }
                session.in_flight = true;
                Some((session.turn_tx.clone(), requested_session_key.clone()))
            } else if parsed.task_id_opt().is_some()
                && let Some(session) = sessions.get_mut(&context_session_key)
            {
                if session.in_flight {
                    return Err(BamlRtError::Conflict(format!(
                        "Concurrent message.sendStream rejected for context {context_id} (session key {context_session_key})"
                    )));
                }
                session.in_flight = true;
                tracing::debug!(
                    context_id = %context_id,
                    requested_key = %requested_session_key,
                    context_key = %context_session_key,
                    "live stream resume matched context-scoped session key"
                );
                Some((session.turn_tx.clone(), context_session_key.clone()))
            } else {
                let (turn_tx, turn_rx) = async_channel::unbounded::<TurnInput>();
                sessions.insert(
                    requested_session_key.clone(),
                    LiveStreamSession {
                        turn_tx: turn_tx.clone(),
                        relay_tx: None,
                        in_flight: true,
                    },
                );
                spawn_payload = Some((requested_session_key.clone(), context_id.clone(), turn_rx));
                Some((turn_tx, requested_session_key.clone()))
            }
        };

        let turn_input = TurnInput {
            request: request.clone(),
            response_tx: LiveResponseSender::new(response_tx),
        };
        if let Some((tx, session_key)) = turn_target
            && tx.send(turn_input).await.is_err()
        {
            {
                let mut sessions = self.stream_sessions.lock().await;
                if let Some(session) = sessions.get_mut(&session_key) {
                    session.in_flight = false;
                }
            }
            return Err(BamlRtError::InvalidArgument(
                "Active stream session closed before input injection".to_string(),
            ));
        }

        if let Some((key, session_context_id, turn_rx)) = spawn_payload {
            let agent = self.clone();
            let span = Span::current().clone();
            tracing::debug!(
                context_id = %session_context_id,
                session_key = %key,
                "live stream: spawning run_live_stream_session task"
            );
            metrics::record_live_stream_event("session_spawn");
            tokio::spawn(
                async move {
                    agent
                        .run_live_stream_session(key, session_context_id, turn_rx)
                        .await;
                }
                .instrument(span),
            );
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
        /// When we break on InputRequired we keep (rx, resume_tx, abort_tx). We drop the turn's
        /// response_tx so the client stream ends and can send the next turn. abort_tx MUST be kept
        /// alive: dropping it closes abort_rx which immediately fires the abort arm in the
        /// collector's tokio::select!, terminating the collector before the resume can arrive.
        type SuspendedState = (
            mpsc::Receiver<(Value, usize, Option<StreamCompletion>)>,
            mpsc::Sender<Value>,
            Option<mpsc::Sender<()>>, // abort_tx — keep alive until stream fully terminates
        );
        let mut suspended: Option<SuspendedState> = None;

        while let Ok(turn) = turn_rx.recv().await {
            let turn_span = spans::live_stream_session_turn(session_context_id.as_str());
            let action = async {
            let turn_started = Instant::now();
            metrics::record_live_stream_event("turn_dequeued");
            tracing::debug!(
                context_id = %session_context_id,
                "live stream: turn dequeued"
            );

            let request_value = turn.request.clone();
            let response_tx = turn.response_tx;

            let (
                mut rx,
                resume_tx_opt,
                abort_tx_opt,
                response_tx,
                request_id,
                session_task_id_str,
                from_resume,
            ) = if let Some((rx, resume_tx, abort_tx)) = suspended.take() {
                // Resume path: send turn request on resume_tx so collector delivers into same JS run; drain same rx.
                // Keep resume_tx and abort_tx so when we hit InputRequired again we can suspend again.
                let request_id = a2a::extract_jsonrpc_id(&turn.request);
                if resume_tx.send(turn.request).await.is_err() {
                    tracing::debug!("resume_tx send failed (collector dropped)");
                    return TurnAction::Break;
                }
                metrics::record_live_stream_event("resume_injected");
                tracing::debug!(
                    context_id = %session_context_id,
                    "live stream resume: sent request on resume_tx, re-entering drain"
                );
                (
                    rx,
                    Some(resume_tx),
                    abort_tx,
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
                let resolved_session_task_id = a2a::A2aRequest::from_value(request_value.clone())
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
                                &BamlRtError::InvalidArgument("live session missing".to_string()),
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
                            return TurnAction::Break;
                        }
                    };
                    // Allow bursty tool/status relay chunks without backpressuring the live session.
                    let (relay_tx, relay_rx) = mpsc::channel(256);
                    session.relay_tx = Some(relay_tx);
                    Some(relay_rx)
                };
                let outcome_inner_start = Instant::now();
                let (request_id, outcome) = match self
                    .handle_a2a_outcome_inner(
                        request_value.clone(),
                        invocation_ctx.clone(),
                        resume_channel,
                        relay_rx,
                    )
                    .instrument(spans::live_stream_outcome_inner(
                        session_context_id.as_str(),
                    ))
                    .await
                {
                    Ok(x) => x,
                    Err(err) => {
                        metrics::record_live_stream_phase_duration(
                            "outcome_inner",
                            outcome_inner_start.elapsed(),
                        );
                        metrics::record_live_stream_event("outcome_inner_err");
                        tracing::debug!(
                            context_id = %session_context_id,
                            error = %err,
                            elapsed_ms = outcome_inner_start.elapsed().as_millis(),
                            "live stream: handle_a2a_outcome_inner returned error"
                        );
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
                        return TurnAction::Break;
                    }
                };
                metrics::record_live_stream_phase_duration(
                    "outcome_inner",
                    outcome_inner_start.elapsed(),
                );
                tracing::debug!(
                    context_id = %session_context_id,
                    elapsed_ms = outcome_inner_start.elapsed().as_millis(),
                    "live stream: handle_a2a_outcome_inner returned ok"
                );

                match outcome {
                    a2a::A2aOutcome::Response(result) => {
                        metrics::record_live_stream_event("outcome_response");
                        let formatted = self.response_formatter.format_success(request_id, result);
                        if response_tx
                            .send(LiveResponseChunk(formatted))
                            .await
                            .is_err()
                        {
                            tracing::debug!("live stream response send failed (receiver dropped)");
                        }
                        return TurnAction::Break;
                    }
                    a2a::A2aOutcome::Stream(handle) => {
                        metrics::record_live_stream_event("outcome_stream");
                        (
                            handle.receiver,
                            handle.resume_tx,
                            handle.abort_tx, // keep alive: dropping closes abort_rx and terminates the collector
                            response_tx,
                            request_id,
                            resolved_session_task_id,
                            false,
                        )
                    }
                }
            };

            // Drain phase: read chunks from the collector and forward to client.
            let mut completion = None;
            let mut last_task_id: Option<String> = None;
            let mut resume_chunk_count: u32 = 0;

            async {
                tracing::debug!(
                    context_id = %session_context_id,
                    from_resume,
                    elapsed_since_turn_start_ms = turn_started.elapsed().as_millis(),
                    "live stream drain started (store_result will run per chunk)"
                );
                let drain_wait_start = Instant::now();
                let mut first_drain_chunk = true;

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
                            completion = Some(StreamCompletion::ChannelClosed);
                            break;
                        }
                        break;
                    };
                    if first_drain_chunk {
                        first_drain_chunk = false;
                        let wait = drain_wait_start.elapsed();
                        metrics::record_live_stream_phase_duration("drain_first_chunk", wait);
                        metrics::record_live_stream_event("first_chunk_from_collector");
                        tracing::debug!(
                            context_id = %session_context_id,
                            from_resume,
                            wait_ms = wait.as_millis(),
                            elapsed_since_turn_start_ms = turn_started.elapsed().as_millis(),
                            "live stream: first chunk from collector (mpsc)"
                        );
                    }
                    let (chunk, index, comp) = outcome_msg;

                    completion = comp;
                    let view = StreamChunkView::new(chunk);
                    if !view.is_null() {
                        if let Some(tid) = view.task_id() {
                            last_task_id = Some(tid.as_str().to_string());
                        }
                        let store_span =
                            spans::live_stream_store_result(index, view.has_storable_payload());
                        async {
                            let raw_for_store = if view.raw.get("__toolStreamChunk").is_some() {
                                let mut c = view.raw.clone();
                                c.as_object_mut()
                                    .and_then(|o| o.remove("__toolStreamChunk"));
                                c
                            } else {
                                view.raw.clone()
                            };
                            let store_result =
                                self.live_result_pipeline.store_result(&raw_for_store).await;
                            let ok = store_result.is_ok();
                            Span::current().record("store_result_ok", ok);
                            if let Err(e) = store_result {
                                tracing::warn!(
                                    error = %e,
                                    "live stream: store_result failed (task/subscribe may miss task)"
                                );
                            }
                        }
                        .instrument(store_span)
                        .await;
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
                            completion = Some(StreamCompletion::ChannelClosed);
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
                            crate::live_stream_working_relay::A2A_RESULT_TOOL_STREAM_CHUNK
                                .to_string(),
                            serde_json::Value::Bool(true),
                        );
                    }
                    if response_tx
                        .send(LiveResponseChunk(formatted))
                        .await
                        .is_err()
                    {
                        tracing::debug!("live stream chunk send failed (receiver dropped)");
                        completion = Some(StreamCompletion::ChannelClosed);
                        break;
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
            }
            .instrument(spans::live_stream_drain(session_context_id.as_str()))
            .await;

            session_task_id = last_task_id
                .or(session_task_id_str)
                .map(|task_id| TaskId::from_external(ExternalId::new(task_id)));

            match completion {
                Some(StreamCompletion::InputRequired) => {
                    if let Some(resume_tx) = resume_tx_opt {
                        // Store (rx, resume_tx, abort_tx). Dropping response_tx closes the client stream so
                        // collect_stream returns and the client can send the next turn. abort_tx MUST be kept:
                        // dropping it closes abort_rx and immediately fires the abort arm in the collector's
                        // tokio::select!, causing the collector to terminate before the resume arrives.
                        suspended = Some((rx, resume_tx, abort_tx_opt));
                    }
                    {
                        let mut sessions = self.stream_sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&session_key) {
                            session.relay_tx = None;
                            session.in_flight = false;
                        }
                    }
                    TurnAction::Continue
                }
                _ => TurnAction::Break,
            }
            }
            .instrument(turn_span)
            .await;

            match action {
                TurnAction::Continue => continue,
                TurnAction::Break => break,
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

        let agent_package_str = self.agent_package.as_str();
        let agent_instance_id_str = self.agent_instance_id.as_str();
        let span = if parsed_request.is_stream() {
            spans::a2a_stream(
                Some(&scope),
                parsed_request.method().as_str(),
                agent_package_str,
                agent_instance_id_str,
                correlation_id.as_str(),
            )
        } else {
            spans::a2a_request(
                Some(&scope),
                parsed_request.method().as_str(),
                agent_package_str,
                agent_instance_id_str,
                correlation_id.as_str(),
            )
        };
        let start = std::time::Instant::now();
        let method = parsed_request.method();
        let invocation = parsed_request.invocation;
        let serving_service_instance_id = baml_rt_observability::service_instance_id();

        let outcome = correlation::with_correlation_id(
            correlation_id,
            async move {
                let invocation_scope = InvocationScope::new(scope.clone());
                context::with_scope(scope, async move {
                    if let a2a::A2aParams::MessageSendStream(params) = &parsed_request.params {
                        let canonical_context_id = invocation_scope.context_id().clone();
                        let receiver_task_id = invocation_scope.task_id_opt().cloned();
                        if let Some(ref task_id) = receiver_task_id {
                            self.task_store
                                .ensure_task_exists(task_id, Some(&canonical_context_id))
                                .await?;
                        }
                        self.task_store
                            .insert_message_for_receiver(
                                &params.message,
                                canonical_context_id,
                                receiver_task_id,
                            )
                            .await?;
                    }
                    self.request_router
                        .route(&parsed_request, &invocation_scope, resume_channel, relay_rx)
                        .instrument(spans::a2a_route(
                            parsed_request.method().as_str(),
                            invocation_scope.context_id().as_str(),
                        ))
                        .await
                })
                .await
            }
            .instrument(span),
        )
        .await;

        let duration = start.elapsed();
        let (context_id_log, task_id_log) = match &resolved_scope {
            RequestScope::MessageScoped { context_id, .. } => (context_id.to_string(), None),
            RequestScope::TaskScoped {
                context_id,
                task_id,
                ..
            } => (context_id.to_string(), Some(task_id.to_string())),
        };

        match &outcome {
            Ok(_) => {
                metrics::record_a2a_request(
                    method.as_str(),
                    agent_package_str,
                    agent_instance_id_str,
                    "success",
                    invocation,
                    serving_service_instance_id,
                    duration,
                );
                tracing::info!(
                    event = "turn_attribution",
                    method = ?method,
                    turn_total_ms = duration.as_millis() as u64,
                    context_id = %context_id_log,
                    task_id = ?task_id_log,
                    result = "success",
                );
            }
            Err(err) => {
                tracing::warn!(error = ?err, "handle_a2a: routing error");
                metrics::record_a2a_request(
                    method.as_str(),
                    agent_package_str,
                    agent_instance_id_str,
                    "error",
                    invocation,
                    serving_service_instance_id,
                    duration,
                );
                metrics::record_a2a_error(
                    method.as_str(),
                    agent_package_str,
                    agent_instance_id_str,
                    self.error_classifier.classify(err),
                    invocation,
                    serving_service_instance_id,
                );
                tracing::info!(
                    event = "turn_attribution",
                    method = ?method,
                    turn_total_ms = duration.as_millis() as u64,
                    context_id = %context_id_log,
                    task_id = ?task_id_log,
                    result = "error",
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

    async fn read(
        &mut self,
        _input: Value,
    ) -> std::result::Result<baml_rt_tools::ToolStep, ToolSessionError> {
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
        BamlRtError,
        context::InvocationScope,
        ids::{ContextId, ExternalId, MessageId, TaskId},
    };
    use baml_rt_provenance::ProvenanceContextReader;
    use serde_json::json;

    use super::{A2aAgent, SurrealRuntimeStore, TaskRepository, synthesized_live_task_id};
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
        let store = baml_rt_provenance::SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("test store");
        let agent = A2aAgent::builder()
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_surreal_store(store)
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
            let mgr = runtime.read().await;
            mgr.execute_tool_with_scope(scope.as_scope(), "js/add", json!({"a": 2, "b": 3}))
                .await
                .expect("execute tool")
        };

        assert_eq!(result.get("sum").and_then(|v| v.as_i64()), Some(5));
    }

    #[tokio::test]
    async fn surreal_runtime_store_insert_message_records_provenance_message_event() {
        let provenance = baml_rt_provenance::SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build store");
        let agent_id = baml_rt_core::ids::AgentId::from_uuid(
            baml_rt_core::ids::UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
        );
        let runtime_store = SurrealRuntimeStore::new(provenance.clone(), agent_id);
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

    #[tokio::test]
    async fn live_send_stream_rejects_concurrent_turn_for_same_context() {
        let store = baml_rt_provenance::SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("test store");
        let agent = A2aAgent::builder()
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_surreal_store(store)
            .build()
            .await
            .expect("agent build");

        let request = json!({
            "jsonrpc": "2.0",
            "id": "conflict-1",
            "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-conflict-1",
                    "role": "ROLE_USER",
                    "parts": [{ "text": "hello" }],
                    "contextId": "ctx-conflict"
                }
            }
        });
        let parsed = crate::a2a::A2aRequest::from_value(request.clone()).expect("parse request");
        let context_key =
            crate::live_stream::LiveStreamSessionKey::from_context_id(parsed.context_id());
        let (turn_tx, _turn_rx) = async_channel::unbounded();
        {
            let mut sessions = agent.stream_sessions.lock().await;
            sessions.insert(
                context_key,
                crate::live_stream::LiveStreamSession {
                    turn_tx,
                    relay_tx: None,
                    in_flight: true,
                },
            );
        }

        let err = match agent.handle_live_message_stream(request, parsed).await {
            Ok(_) => panic!("concurrent stream should be rejected"),
            Err(err) => err,
        };
        match err {
            BamlRtError::Conflict(msg) => {
                assert!(
                    msg.contains("Concurrent message.sendStream rejected"),
                    "unexpected conflict message: {msg}"
                );
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    /// Full-pipeline history test: events → provenance store → conversation_context
    /// → to_projection_item → project_prompt_context → rendered JSON.
    ///
    /// This exercises the exact path used by `ctx.tags['conversation_history']` in BAML
    /// prompts: the same call sequence as `ProjectingConversationContextProvider::conversation_history_json`.
    #[tokio::test]
    async fn session_history_renders_correctly_through_full_pipeline() {
        use baml_rt_conversation::view::SessionStepOp;
        use baml_rt_core::ids::{AgentId, ExternalId, MessageId, UuidId};
        use baml_rt_provenance::{
            CallScope, ProvEvent, ProvenanceContextReader, ProvenanceWriter, SurrealStoreBuilder,
        };
        use baml_rt_tools::{
            ToolRegistry,
            archive_read::{ShortRef, render_to_lines},
            archive_refs::ArchiveEntry,
            prompt_projection::project_prompt_context,
        };

        // build() returns Arc<SurrealProvenanceStore> — do not double-wrap.
        let store = SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build isolated test store");
        let context_id = ContextId::new(1, 1);
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap());

        // User message — mirrors what arrives via A2A task handling.
        store
            .add_event(ProvEvent::message_received_global(
                context_id.clone(),
                MessageId::from_external(ExternalId::new("msg-1")),
                "user".into(),
                vec!["what can you do".into()],
                None,
                agent_id.clone(),
                1_700_000_000_000,
            ))
            .await
            .expect("message_received");

        let scope = CallScope::Message {
            message_id: MessageId::from_external(ExternalId::new("msg-1")),
        };
        let session_id = "session-abc123".to_string();
        let tool_name = "system/discover_agents".to_string();

        // Open — LLM chose to open the discover_agents session.
        store
            .add_event(ProvEvent::tool_session_step(
                context_id.clone(),
                scope.clone(),
                tool_name.clone(),
                session_id.clone(),
                &SessionStepOp::Open,
            ))
            .await
            .expect("session open");

        // SendDone — blocking Send completed; result archived at @1.
        // Header is derived the same way production code does it: via ArchiveEntry::display_header.
        let short_ref = ShortRef::new(1);
        let archive_ref = short_ref.to_string(); // "@1"
        let result_payload = serde_json::json!([
            {"name": "crm-agent", "description": "Business reporting agent"},
            {"name": "dev-agent", "description": "Code generation agent"},
        ]);
        let entry = ArchiveEntry::new(
            render_to_lines(&result_payload),
            tool_name.clone(),
            "found 2 agents".into(),
            String::new(),
            "tool_result".to_string(),
        );
        let header = entry.display_header(short_ref);
        store
            .add_event(ProvEvent::tool_session_step(
                context_id.clone(),
                scope.clone(),
                tool_name.clone(),
                session_id.clone(),
                &SessionStepOp::SendDone {
                    archive_ref: archive_ref.clone(),
                    header: header.clone(),
                    informed_by: "test-anchor-1".to_string(),
                },
            ))
            .await
            .expect("session send_done");

        // Read — LLM requested a grep of the archived result.
        store
            .add_event(ProvEvent::tool_session_step(
                context_id.clone(),
                scope.clone(),
                tool_name.clone(),
                session_id.clone(),
                &SessionStepOp::SearchRead {
                    archive_ref: archive_ref.clone(),
                    grep: "name description".into(),
                    offset: 0,
                    limit: 200,
                },
            ))
            .await
            .expect("session read");

        // --- Pipeline: store → to_projection_item → project_prompt_context ---
        let raw_items = store
            .conversation_context(&context_id, None)
            .await
            .expect("conversation_context");

        let projection_items: Vec<_> = raw_items
            .into_iter()
            .filter_map(super::to_projection_item)
            .collect();

        let registry = ToolRegistry::new();
        let ref_table = baml_rt_tools::archive_refs::RefTable::new();
        // No archive reader — SearchRead shows the grep analogue (`grep -n '…' @1`), pud-squashed.
        let history = project_prompt_context(projection_items, &registry, &ref_table, None);
        let items = history.as_array().expect("array");

        // 4 items: user message + Open + SendDone + SearchRead
        // (no ToolCall/ToolResult — only the user line plus three session steps)
        assert_eq!(items.len(), 4, "expected 4 history items, got: {history}");

        // Roles in `conversation_history` are canonical chat labels: messages map to
        // `user` / `assistant` (see `conversation_history_role_for_message`), and
        // tool/session rows use `tool` (see prompt_projection module docs).
        // [0] user message — citation-aware projection allocates `#1` for the first history line
        // (see `prompt_projection::render_content` Message branch).
        let user_role = items[0]["role"].as_str().unwrap();
        assert!(
            user_role.contains("USER") || user_role == "user",
            "expected user role, got: {user_role}"
        );
        assert_eq!(
            items[0]["content"].as_str().unwrap(),
            "#1 what can you do",
            "user content should be history-ref prefixed for drift/citation resolution"
        );

        // [1] Open: describes the session being opened. Session rows use role `tool`.
        assert_eq!(items[1]["role"], "tool");
        let open_content = items[1]["content"].as_str().unwrap();
        assert!(
            open_content.contains("discover_agents"),
            "Open should mention tool name, got: {open_content}"
        );

        // [2] SendDone: the header IS the display ("@1 tool 'summary' [...]")
        // No double @1 prefix — header already starts with the archive ref.
        assert_eq!(items[2]["role"], "tool");
        let send_content = items[2]["content"].as_str().unwrap();
        assert_eq!(
            send_content, header,
            "SendDone content should be exactly the header, got: {send_content}"
        );
        assert!(
            send_content.starts_with("@1"),
            "SendDone must start with archive ref, got: {send_content}"
        );

        // [3] SearchRead: grep analogue (pud-squashed); with reader would be paginated output only.
        assert_eq!(items[3]["role"], "tool");
        let read_content = items[3]["content"].as_str().unwrap();
        assert_eq!(
            read_content, "grep -n 'name description' @1",
            "Read without archive_reader must show grep command line"
        );
    }
}
