//! BAML Agent Runner
//!
//! This binary loads and executes one or more packaged agent applications.
//! Each agent package is a tar.gz containing BAML schemas, compiled TypeScript,
//! and metadata.

#![recursion_limit = "256"]

mod builder;
mod package;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Context;
use async_trait::async_trait;
use baml_rt_a2a::{
    A2aAgent, A2aRequestHandler, AgentRegistry, a2a,
    a2a_types::{
        A2aMessageId, JSONRPCId, JSONRPCRequest, Message, MessageRole, Part,
        SendMessageConfiguration, SendMessageRequest,
    },
};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentInstanceId, AgentLister,
    AgentManifest, AgentPackageName, AgentRouteKey, BamlRtError, ContextId, Result, RuntimeScope,
    bus::BusStream,
    collect_a2a_stream,
    context::{self, InvocationScope},
    ids::{AgentId, DerivedId, ExternalId, TaskId},
    route_key_from_request,
};
use baml_rt_observability::{spans, tracing_setup};
use baml_rt_provenance::{
    AgentType, GraphExporter, GraphQueryParams, GraphStore, GraphqliteStoreBuilder, ProvEvent,
    ProvenanceWriter, ToolIndexConfig, context_metrics_queries,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
    index_tools,
};
use baml_rt_quickjs::BamlRuntimeManager;
use baml_rt_tools::{
    ManifestToolNames, ToolAccessPolicy, parse_access_allowlist, register_manifest_tools,
};
use baml_rt_tools_claude::{AgentWorkspaceRegistry, ClaudeSessionBundle};
use baml_tools_calculator as _;
#[cfg(feature = "clickup")]
use baml_tools_clickup as _;
#[cfg(feature = "memory")]
use baml_tools_memory as _;
#[cfg(feature = "notion")]
use baml_tools_notion as _;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use baml_tools_system::SystemBundle;
use clap::Parser;
use serde_json::{Map, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tracing::{error, info, warn};

/// Inert agent package - just holds package data
pub(crate) struct AgentPackage {
    manifest: AgentManifest,
    extract_dir: PathBuf,
    baml_src: PathBuf,
}

/// Boot typestate: schema loaded into runtime manager.
struct SchemaLoaded {
    runtime_manager: BamlRuntimeManager,
}

/// Boot typestate: manifest + host system tools registered, allowlist enforced.
struct ToolsRegistered {
    runtime_manager: BamlRuntimeManager,
}

/// Boot typestate: A2A agent built and JS initialized.
struct JsInitialized {
    runtime_manager: Arc<Mutex<BamlRuntimeManager>>,
    agent: A2aAgent,
}

impl AgentPackage {
    /// Load an agent package from a tar.gz file (inert - does not boot the agent)
    pub(crate) async fn load_from_file(package_path: &Path) -> Result<Self> {
        let (extract_dir, manifest) = package::load_package(package_path).await?;
        let baml_src = extract_dir.join("baml_src");
        Ok(Self {
            manifest,
            extract_dir,
            baml_src,
        })
    }

    async fn load_schema_phase(&self) -> Result<SchemaLoaded> {
        let mut runtime_manager = BamlRuntimeManager::new()?;
        let schema_span = spans::load_baml_schema(&self.baml_src);
        let _schema_guard = schema_span.enter();
        let baml_src_str = self.baml_src.to_str().ok_or_else(|| {
            BamlRtError::InvalidArgument("BAML source path contains invalid UTF-8".to_string())
        })?;
        runtime_manager.load_schema(baml_src_str)?;
        info!(agent = self.manifest.name, "BAML schema loaded");
        Ok(SchemaLoaded { runtime_manager })
    }

    async fn register_tools_phase(
        &self,
        loaded: SchemaLoaded,
        policy: &ToolAccessPolicy,
        agent_list_catalogue: Arc<dyn AgentLister>,
        a2a_handler: Arc<dyn A2aRequestHandler>,
    ) -> Result<ToolsRegistered> {
        let runtime_manager = loaded.runtime_manager;
        let manifest_tool_names = ManifestToolNames::parse(&self.manifest.tools)?;

        // Host composes tool catalogue:
        // - system bundle (internal_a2a, discover_agents, discover_tools)
        // - claude bundle (claude/dev host-managed stream session)
        let tool_registry = runtime_manager.tool_registry();
        tool_registry.register_bundle(SystemBundle::new(
            agent_list_catalogue,
            tool_registry.clone(),
            a2a_handler,
        ))?;
        // Claude workspace root: where claude/dev session cwd lives (agent_id/workspace_name subdirs).
        // Set BAML_CLAUDE_WORKSPACES_BASE to a persistent dir so tmp isn't wiped and Claude can write.
        let claude_workspace_root = match std::env::var("BAML_CLAUDE_WORKSPACES_BASE") {
            Ok(ref base) if !base.trim().is_empty() => {
                let path = PathBuf::from(base.trim());
                let absolute = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("."))
                        .join(path)
                };
                std::fs::create_dir_all(&absolute).map_err(BamlRtError::Io)?;
                let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
                info!(
                    env = base.trim(),
                    base = %canonical.display(),
                    "Claude workspaces root from BAML_CLAUDE_WORKSPACES_BASE (persistent)",
                );
                canonical
            }
            _ => {
                let fallback = self.extract_dir.join(".claude-workspaces");
                info!(
                    base = %fallback.display(),
                    "Claude workspaces root under extract dir (BAML_CLAUDE_WORKSPACES_BASE unset or empty). Set BAML_CLAUDE_WORKSPACES_BASE (e.g. in .env in current working directory) to use a persistent workspace root.",
                );
                fallback
            }
        };
        tool_registry.register_bundle(ClaudeSessionBundle::new(Arc::new(
            AgentWorkspaceRegistry::new(claude_workspace_root),
        )))?;

        #[cfg(feature = "memory")]
        if self.manifest.tools.iter().any(|t| t.starts_with("memory/")) {
            let memory_bundle = baml_tools_memory::MemoryBundle::new(&self.manifest.name)?;
            tool_registry.register_bundle(memory_bundle)?;
        }

        register_manifest_tools(
            runtime_manager.tool_registry().as_ref(),
            &manifest_tool_names,
            policy,
        )?;

        // Apply allowlist after host bundle registration so system/* tools are optional
        // unless explicitly declared in the agent manifest.
        runtime_manager
            .set_tool_allowlist(self.manifest.tools.iter().cloned().collect::<HashSet<_>>())
            .await?;

        Ok(ToolsRegistered { runtime_manager })
    }

    async fn build_agent_phase(
        &self,
        registered: ToolsRegistered,
        provenance_config: &ProvenanceConfig,
        stream_idle_secs: Option<u64>,
    ) -> Result<JsInitialized> {
        use baml_rt_quickjs::QuickJSConfig;

        let runtime_manager_arc = Arc::new(Mutex::new(registered.runtime_manager));
        let quickjs_config = QuickJSConfig::new().with_stream_collector_idle_secs(stream_idle_secs);
        let mut agent_builder = A2aAgent::builder()
            .with_runtime_handle(runtime_manager_arc.clone())
            .with_quickjs_config(quickjs_config)
            .with_baml_helpers(true)
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()));

        agent_builder = agent_builder.with_graphqlite_store(provenance_config.store().clone());

        let agent = agent_builder.build().await?;
        Ok(JsInitialized {
            runtime_manager: runtime_manager_arc,
            agent,
        })
    }

    async fn initialize_js_phase(&self, built: JsInitialized) -> Result<JsInitialized> {
        let entry_point_path = self.extract_dir.join(&self.manifest.entry_point);
        if entry_point_path.exists() {
            let eval_span = spans::evaluate_agent_code(&self.manifest.entry_point);
            let _eval_guard = eval_span.enter();

            let agent_code = std::fs::read_to_string(&entry_point_path).map_err(BamlRtError::Io)?;

            info!(
                entry_point = self.manifest.entry_point,
                "Loading agent JavaScript code"
            );

            let bridge = built.agent.bridge();
            let mut bridge_guard = bridge.lock().await;
            match bridge_guard.evaluate(None, &agent_code).await {
                Ok(_) => info!("Agent code executed successfully"),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Agent code execution returned an error (may be expected)"
                    );
                }
            }

            info!("Agent JavaScript code loaded and initialized");
        } else {
            info!(
                entry_point = self.manifest.entry_point,
                "Agent entry point not found, skipping JavaScript initialization"
            );
        }

        Ok(built)
    }

    /// Boot this package into a running A2aAgent.
    ///
    /// The host (runner) composes the tool catalogue at startup: tools live in crates/tools
    /// and are registered here. The relationship between agent and tools is indirect — mediated by the host.
    pub(crate) async fn boot(
        &self,
        provenance_config: &ProvenanceConfig,
        tool_index: Option<ToolIndexConfig>,
        policy: &ToolAccessPolicy,
        agent_list_catalogue: Arc<dyn AgentLister>,
        a2a_handler: Arc<dyn A2aRequestHandler>,
        stream_idle_secs: Option<u64>,
    ) -> Result<(A2aAgent, AgentId)> {
        let span = spans::load_agent_package(&self.extract_dir);
        let _guard = span.enter();
        let loaded = self.load_schema_phase().await?;
        let registered = self
            .register_tools_phase(loaded, policy, agent_list_catalogue, a2a_handler)
            .await?;
        let built = self
            .build_agent_phase(registered, provenance_config, stream_idle_secs)
            .await?;
        let initialized = self.initialize_js_phase(built).await?;
        let agent = initialized.agent;
        let runtime_manager_arc = initialized.runtime_manager;

        if let Some(index_config) = tool_index {
            let manager = runtime_manager_arc.lock().await;
            let tools = manager.export_tool_metadata().await;
            if let Err(err) = index_tools(&index_config, &tools).await {
                warn!(error = %err, "Failed to index tool metadata in GraphQLite");
            } else {
                info!("Tool metadata indexed in GraphQLite");
            }
        }

        // Get agent_id from the agent (generated during A2aAgent::build())
        let agent_id = agent.agent_id().clone();

        // Emit AgentBooted provenance event (provenance store is always present).
        let writer = provenance_config.store().clone() as Arc<dyn ProvenanceWriter>;
        let archive_path = self.manifest.signature.clone();
        let agent_type_parsed = AgentType::new(self.manifest.name.clone()).ok_or_else(|| {
            BamlRtError::InvalidArgument("agent_type cannot be empty".to_string())
        })?;
        let boot_event = ProvEvent::agent_booted(
            agent_id.clone(),
            agent_type_parsed,
            self.version().to_string(),
            archive_path,
        );
        if let Err(e) = writer.add_event(boot_event).await {
            error!(error = ?e, agent_id = %agent_id, "Failed to write AgentBooted event to provenance store");
        } else {
            info!(agent_id = %agent_id, "AgentBooted event written to provenance store");
        }

        Ok((agent, agent_id))
    }

    /// Get the agent name
    fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Get the manifest version (used for provenance AgentBooted and discovery card).
    fn version(&self) -> &str {
        &self.manifest.version
    }

    /// Get the full manifest (for discovery card derivation)
    fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

/// Booted agent - holds the running A2aAgent and full manifest for discovery.
#[derive(Clone)]
pub(crate) struct BootedAgent {
    agent: A2aAgent,
    manifest: AgentManifest,
}

impl BootedAgent {
    /// Manifest version (for discovery card and listing).
    fn version(&self) -> &str {
        &self.manifest.version
    }

    async fn invoke_function(&self, function_name: &str, args: Value) -> Result<Value> {
        let scope = InvocationScope::synthetic_message(self.agent.agent_id().clone());
        let bridge = self.agent.bridge();
        let mut js_bridge = bridge.lock().await;
        js_bridge
            .invoke_js_function(&scope, function_name, args)
            .await
    }

    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.agent.handle_a2a_stream(request).await
    }
}

/// Agent runner (host) that manages agents and composes the tool catalogue at startup.
/// Tools live in crates/tools; the relationship between an agent and its tools is indirect — mediated by the host.
/// Uses interior mutability for agents so the runner can be shared as Arc before loading completes.
pub(crate) struct AgentRunner {
    agents: RwLock<HashMap<String, BootedAgent>>,
    provenance_config: ProvenanceConfig,
    tool_index: Option<ToolIndexConfig>,
    access_policy: ToolAccessPolicy,
    routed_agents: std::sync::RwLock<HashMap<AgentRouteKey, A2aAgent>>,
    internal_a2a_router: Arc<InternalA2aRouter>,
    stream_idle_secs: Option<u64>,
}

impl AgentRunner {
    pub(crate) fn new(
        provenance_config: ProvenanceConfig,
        tool_index: Option<ToolIndexConfig>,
        access_policy: ToolAccessPolicy,
        stream_idle_secs: Option<u64>,
    ) -> Self {
        let routed_agents = std::sync::RwLock::new(HashMap::new());
        let internal_a2a_router = Arc::new(InternalA2aRouter::new());
        Self {
            agents: RwLock::new(HashMap::new()),
            provenance_config,
            tool_index,
            access_policy,
            routed_agents,
            internal_a2a_router,
            stream_idle_secs,
        }
    }

    pub(crate) fn provenance_config(&self) -> &ProvenanceConfig {
        &self.provenance_config
    }

    pub(crate) fn tool_index(&self) -> &Option<ToolIndexConfig> {
        &self.tool_index
    }

    pub(crate) fn access_policy(&self) -> &ToolAccessPolicy {
        &self.access_policy
    }

    pub(crate) fn stream_idle_secs(&self) -> Option<u64> {
        self.stream_idle_secs
    }

    /// Get the internal A2A router (used by builder to create scoped routers).
    pub(crate) fn internal_a2a_router(&self) -> &Arc<InternalA2aRouter> {
        &self.internal_a2a_router
    }

    /// Insert a booted agent (used by builder during load phase).
    pub(crate) fn insert_agent(&self, name: String, route_key: AgentRouteKey, booted: BootedAgent) {
        {
            let mut routed = self.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key, booted.agent.clone());
        }
        let mut guard = self.agents.write().expect("RwLock poison");
        guard.insert(name.clone(), booted);
        let count = guard.len();
        drop(guard);
        tracing::info!(agent = %name, total_agents = count, "Runner: agent inserted (discovery will see this count)");
    }

    /// Execute a function in a specific agent
    pub(crate) async fn invoke(
        &self,
        agent_name: &str,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        let span = spans::invoke_function(None, agent_name, function_name);
        let _guard = span.enter();

        let agent = {
            let agents = self.agents.read().expect("RwLock poison");
            agents.get(agent_name).cloned().ok_or_else(|| {
                BamlRtError::AgentNotFound(format!("Agent '{agent_name}' not found"))
            })?
        };
        agent.invoke_function(function_name, args).await
    }

    /// List loaded agent names (for CLI display).
    pub(crate) fn list_agents(&self) -> Vec<String> {
        self.agents
            .read()
            .expect("RwLock poison")
            .keys()
            .cloned()
            .collect()
    }

    /// List running agents as discovery entries (for HTTP GET /agents).
    pub(crate) fn discovery_entries(&self) -> Vec<AgentDiscoveryEntry> {
        self.agents
            .read()
            .expect("RwLock poison")
            .iter()
            .map(|(pkg, booted)| {
                let m = &booted.manifest;
                let version = booted.version().to_string();
                let agent_card = AgentCard {
                    name: m.name.clone(),
                    version: version.clone(),
                    agent_package: pkg.clone(),
                    agent_instance_id: AgentInstanceId::DEFAULT.to_string(),
                    tools: m.tools.clone(),
                    description: m.discovery.as_ref().and_then(|d| d.description.clone()),
                    capabilities: m
                        .discovery
                        .as_ref()
                        .map(|d| d.capabilities.clone())
                        .unwrap_or_default(),
                };
                AgentDiscoveryEntry {
                    agent_package: pkg.clone(),
                    agent_instance_id: AgentInstanceId::DEFAULT.to_string(),
                    name: m.name.clone(),
                    version,
                    agent_card,
                }
            })
            .collect()
    }

    /// Handle A2A request by route key (for HTTP POST /agents/.../a2a).
    ///
    /// Uses scope derived from the request's context_id so coordinator and delegated flow
    /// share one context. Avoids synthetic_message which generates a fresh context_id and
    /// would cause the initial user message to land in a different context than the agent's.
    pub(crate) async fn handle_a2a_by_key(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let routed_agent = {
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents.get(key).cloned().ok_or_else(|| {
                BamlRtError::AgentNotFound(format!(
                    "Agent {agent_package}/{agent_instance_id} not found",
                    agent_package = key.agent_package.as_str(),
                    agent_instance_id = key.agent_instance_id.as_str()
                ))
            })?
        };
        let scope = scope_from_request(request.as_ref(), routed_agent.agent_id().clone());
        context::with_scope(scope.as_scope().clone(), async move {
            routed_agent.handle_a2a_stream(request).await
        })
        .await
    }

    /// Run the A2A JSON-RPC loop over the given reader/writer (one JSON-RPC request per line).
    /// Enables tests to use in-memory buffers instead of stdin/stdout.
    pub(crate) async fn run_a2a_loop<R, W>(&self, reader: R, mut writer: W) -> Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: AsyncWriteExt + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let mut request_value: Value = match serde_json::from_str::<Value>(line) {
                Ok(value) if value.is_object() => value,
                Ok(_) => wrap_plaintext_message(line)?,
                Err(_) => wrap_plaintext_message(line)?,
            };

            let request_id = a2a::extract_jsonrpc_id(&request_value);
            let (agent_name, prepared_request) = match self.prepare_a2a_request(&mut request_value)
            {
                Ok(result) => result,
                Err(err) => {
                    let response = map_a2a_error(request_id, err);
                    let serialized = serde_json::to_string(&response)
                        .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string());
                    writer.write_all(serialized.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    continue;
                }
            };

            let agent = {
                let agents = self.agents.read().expect("RwLock poison");
                agents.get(&agent_name).cloned()
            };
            let agent = if let Some(agent) = agent {
                agent
            } else {
                let response = a2a::error_response(
                    request_id,
                    -32601,
                    "Agent not found",
                    Some(Value::String(agent_name)),
                );
                let serialized = serde_json::to_string(&response)
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string());
                writer.write_all(serialized.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                continue;
            };

            let method = request_value
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let correlation_id = request_id
                .as_ref()
                .map(|id| format!("{:?}", id))
                .unwrap_or_else(|| "none".to_string());
            let span = spans::a2a_stdio_request(&agent_name, method, &correlation_id);
            let _guard = span.enter();

            let responses: Vec<Value> = match agent
                .handle_a2a_stream(A2aWireRequest::from(prepared_request))
                .await
            {
                Ok(stream) => collect_a2a_stream(stream)
                    .await
                    .into_iter()
                    .map(A2aStreamChunk::into_inner)
                    .collect(),
                Err(err) => vec![map_a2a_error(request_id, err)],
            };
            for response in responses {
                let serialized = serde_json::to_string(&response)
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string());
                writer.write_all(serialized.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
            writer.flush().await?;
        }

        Ok(())
    }

    pub(crate) async fn run_a2a_stdio(&self) -> Result<()> {
        use tokio::io;
        let stdin = io::stdin();
        let stdout = io::stdout();
        self.run_a2a_loop(io::BufReader::new(stdin), stdout).await
    }

    pub(crate) fn prepare_a2a_request(&self, request: &mut Value) -> Result<(String, Value)> {
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BamlRtError::InvalidArgument("A2A request missing method".to_string()))?
            .to_string();

        let agents = self.agents.read().expect("RwLock poison");

        if is_a2a_method(&method) {
            let agent_name = a2a::extract_agent_name(request).or_else(|| {
                request
                    .get("params")
                    .and_then(|params| params.get("agent"))
                    .and_then(|agent| agent.as_str())
                    .map(|agent| agent.to_string())
            });
            if let Some(agent_name) = agent_name {
                return Ok((agent_name, request.clone()));
            }
            if let Some(agent_name) = select_implicit_stdio_agent(&agents) {
                return Ok((agent_name, request.clone()));
            }
            return Err(BamlRtError::InvalidArgument(
                "A2A request missing agent (set message metadata agent or params.agent)"
                    .to_string(),
            ));
        }

        let obj = request.as_object_mut().ok_or_else(|| {
            BamlRtError::InvalidArgument("A2A request must be a JSON object".to_string())
        })?;
        let (method_base, had_stream_suffix) = strip_stream_suffix(&method);
        let params_value = obj.remove("params").unwrap_or(Value::Null);
        let mut params = match params_value {
            Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("value".to_string(), other);
                map
            }
        };

        let agent_name = if let Some(agent_value) = params.remove("agent") {
            agent_value.as_str().map(|s| s.to_string())
        } else {
            None
        };

        let (agent_name, method_name) = if let Some(agent_name) = agent_name {
            (agent_name, method_base)
        } else if let Some((agent_name, method_name)) = split_agent_method(&method_base, &agents) {
            (agent_name, method_name)
        } else if let Some(agent_name) = select_implicit_stdio_agent(&agents) {
            (agent_name, method_base)
        } else {
            return Err(BamlRtError::InvalidArgument(
                "A2A request missing agent (set params.agent or prefix method with agent name)"
                    .to_string(),
            ));
        };

        if had_stream_suffix {
            params.insert("stream".to_string(), Value::Bool(true));
        }

        if (method_name == "message.send" || method_name == "message.sendStream")
            && let Some(message_value) = params.get_mut("message")
            && message_value.is_object()
            && let Some(message_obj) = message_value.as_object_mut()
        {
            let metadata_entry = message_obj
                .entry("metadata".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(meta_obj) = metadata_entry {
                meta_obj
                    .entry("agent".to_string())
                    .or_insert_with(|| Value::String(agent_name.clone()));
            }
        }

        obj.insert("method".to_string(), Value::String(method_name));
        obj.insert("params".to_string(), Value::Object(params));

        Ok((agent_name, request.clone()))
    }
}

/// Thin wrapper so we can pass the runner as `Arc<dyn AgentRegistry>` to the HTTP API.
pub(crate) struct RunnerRegistry(pub(crate) Arc<AgentRunner>);

impl AgentLister for RunnerRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let entries = self.0.discovery_entries();
        tracing::info!(
            count = entries.len(),
            "Discovery list_agents called (same registry as HTTP GET /agents)"
        );
        entries
    }
}

#[async_trait]
impl AgentRegistry for RunnerRegistry {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.0.handle_a2a_by_key(key, request).await
    }
}

#[async_trait]
impl A2aRequestHandler for RunnerRegistry {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let key = route_key_from_request(&request)?;
        self.0.handle_a2a_by_key(&key, request).await
    }
}

#[derive(Clone)]
pub(crate) struct InternalA2aRouter {
    /// Shared view of routed agents — populated at startup, read-only during serving.
    /// Uses the runner's own `routed_agents` lock via a reference.
    runner: std::sync::OnceLock<Arc<AgentRunner>>,
}

impl InternalA2aRouter {
    fn new() -> Self {
        Self {
            runner: std::sync::OnceLock::new(),
        }
    }

    /// Wire to the runner after construction (called once by the builder).
    pub(crate) fn set_runner(&self, runner: Arc<AgentRunner>) {
        if self.runner.set(runner).is_err() {
            tracing::warn!(
                "InternalA2aRouter::set_runner called after runner already set; duplicate wiring ignored"
            );
        }
    }

    async fn route_from(
        &self,
        caller: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let runner = self
            .runner
            .get()
            .expect("InternalA2aRouter: runner not set");

        // Trust model: in-process agents are trusted peers, so any loaded package may route
        // to any other loaded package via system/internal_a2a.
        let key = extract_internal_a2a_target(request.as_ref())
            .or_else(|| {
                a2a::extract_agent_name(request.as_ref()).and_then(|agent_package| {
                    AgentPackageName::parse(agent_package)
                        .map(|pkg| AgentRouteKey::new(pkg, AgentInstanceId::default()))
                })
            })
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "system/internal_a2a missing params.metadata.target.agent_package".to_string(),
                )
            })?;

        if key.agent_instance_id.as_str() != AgentInstanceId::DEFAULT {
            return Err(BamlRtError::InvalidArgument(format!(
                "system/internal_a2a only supports agent_instance_id=default, got '{agent_instance_id}'",
                agent_instance_id = key.agent_instance_id.as_str()
            )));
        }

        if key == *caller {
            return Err(BamlRtError::InvalidArgument(
                "system/internal_a2a self-routing is not allowed".to_string(),
            ));
        }

        let routed_agent = {
            let agents = runner.routed_agents.read().expect("RwLock poison");
            agents.get(&key).cloned().ok_or_else(|| {
                BamlRtError::AgentNotFound(format!(
                    "Agent {agent_package}/{agent_instance_id} not found",
                    agent_package = key.agent_package.as_str(),
                    agent_instance_id = key.agent_instance_id.as_str()
                ))
            })?
        };

        // INVARIANT: internal_a2a must NEVER set its own context. The request carries the
        // caller's context_id (from build_send_stream_request); the child agent parses it
        // and sets scope from the request. Wrap in scope_from_request so the child runs
        // with the request's context_id even if any code path reads the thread-local scope.
        let scope = scope_from_request(request.as_ref(), routed_agent.agent_id().clone());
        context::with_scope(scope.as_scope().clone(), async move {
            routed_agent.handle_a2a_stream(request).await
        })
        .await
    }
}

#[derive(Clone)]
pub(crate) struct ScopedInternalA2aRouter {
    caller: AgentRouteKey,
    router: Arc<InternalA2aRouter>,
}

impl ScopedInternalA2aRouter {
    pub(crate) fn new(caller: AgentRouteKey, router: Arc<InternalA2aRouter>) -> Self {
        Self { caller, router }
    }
}

/// Build scope from request so coordinator and delegated flow share one context_id.
/// Uses request's context_id when parseable; falls back to synthetic for non-A2A or malformed requests.
fn scope_from_request(request: &serde_json::Value, agent_id: AgentId) -> InvocationScope {
    match a2a::A2aRequest::from_value(request.clone()) {
        Ok(parsed) => InvocationScope::new(RuntimeScope::from_request_scope(
            &parsed.resolved_scope,
            agent_id,
        )),
        Err(_) => InvocationScope::synthetic_message(agent_id),
    }
}

fn extract_internal_a2a_target(request: &Value) -> Option<AgentRouteKey> {
    let params = request.get("params")?.as_object()?;
    let metadata = params.get("metadata")?.as_object()?;
    let target = metadata.get("target")?.as_object()?;
    let agent_package = AgentPackageName::parse(target.get("agent_package")?.as_str()?)?;
    let agent_instance_id = AgentInstanceId::parse(
        target
            .get("agent_instance_id")
            .and_then(Value::as_str)
            .unwrap_or(AgentInstanceId::DEFAULT),
    )?;
    Some(AgentRouteKey::new(agent_package, agent_instance_id))
}

#[async_trait]
impl A2aRequestHandler for ScopedInternalA2aRouter {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.router.route_from(&self.caller, request).await
    }
}

fn strip_stream_suffix(method: &str) -> (String, bool) {
    for suffix in ["/stream", ".stream", ":stream"] {
        if let Some(stripped) = method.strip_suffix(suffix) {
            return (stripped.to_string(), true);
        }
    }
    (method.to_string(), false)
}

fn split_agent_method(
    method: &str,
    agents: &HashMap<String, BootedAgent>,
) -> Option<(String, String)> {
    for sep in ["::", "/", "."] {
        if let Some((prefix, suffix)) = method.split_once(sep)
            && agents.contains_key(prefix)
        {
            return Some((prefix.to_string(), suffix.to_string()));
        }
    }
    None
}

fn select_implicit_stdio_agent(agents: &HashMap<String, BootedAgent>) -> Option<String> {
    if agents.len() == 1 {
        return agents.keys().next().cloned();
    }

    // Preserve backwards-compatible plaintext routing for multi-agent stdio sessions.
    if agents.contains_key("coordinator-agent") {
        return Some("coordinator-agent".to_string());
    }

    None
}

fn is_a2a_method(method: &str) -> bool {
    method.starts_with("message/") || method.starts_with("tasks/") || method.starts_with("agent/")
}

fn map_a2a_error(id: Option<JSONRPCId>, err: BamlRtError) -> Value {
    match err {
        BamlRtError::AgentNotFound(message) => {
            a2a::error_response(id, -32601, "Agent not found", Some(Value::String(message)))
        }
        BamlRtError::InvalidArgument(message) => {
            a2a::error_response(id, -32602, "Invalid params", Some(Value::String(message)))
        }
        BamlRtError::FunctionNotFound(message) => {
            a2a::error_response(id, -32601, "Method not found", Some(Value::String(message)))
        }
        BamlRtError::QuickJs(message) => {
            a2a::error_response(id, -32000, "QuickJS error", Some(Value::String(message)))
        }
        other => a2a::error_response(
            id,
            -32603,
            "Internal error",
            Some(Value::String(other.to_string())),
        ),
    }
}

static MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(1);
static STDIO_CONTEXT_ID: std::sync::OnceLock<ContextId> = std::sync::OnceLock::new();
static STDIO_TASK_ID: std::sync::OnceLock<TaskId> = std::sync::OnceLock::new();

fn stdio_context_id() -> ContextId {
    STDIO_CONTEXT_ID
        .get_or_init(context::generate_context_id)
        .clone()
}

fn stdio_task_id() -> TaskId {
    STDIO_TASK_ID
        .get_or_init(|| {
            let context_id = stdio_context_id();
            TaskId::from_external(ExternalId::new(format!(
                "cli-task-{context_id}",
                context_id = context_id.as_str()
            )))
        })
        .clone()
}

fn wrap_plaintext_message(text: &str) -> Result<Value> {
    let seq = MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let message_id = A2aMessageId::outgoing(DerivedId::new(format!("cli-msg-{seq}")));
    let message = Message {
        message_id,
        role: MessageRole::User,
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: Some(stdio_context_id()),
        task_id: Some(stdio_task_id()),
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    };
    let params = SendMessageRequest {
        message,
        configuration: Some(SendMessageConfiguration {
            blocking: Some(true),
            ..Default::default()
        }),
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap_or(Value::Null)),
        id: Some(JSONRPCId::Null),
    };
    serde_json::to_value(request)
        .map_err(|e| BamlRtError::InvalidArgument(format!("Failed to build stdio request: {e}")))
}

/// Provenance DB: in-memory (default) or file-backed SQLite. No FalkorDB.
#[derive(Debug, Clone)]
enum ProvenanceDb {
    InMemory,
    File(PathBuf),
}

#[derive(Debug, Clone)]
struct RunnerConfig {
    packages: Vec<PathBuf>,
    invoke: Option<(String, String, String)>,
    a2a_stdio: bool,
    serve_http: Option<String>,
    web_dir: Option<PathBuf>,
    provenance_db: ProvenanceDb,
    /// If set, used as Claude workspaces root (overrides BAML_CLAUDE_WORKSPACES_BASE env).
    claude_workspaces_base: Option<PathBuf>,
    /// Stream collector idle timeout in seconds. No yield for this long ends the stream (Timeout). Default 600 for long-running tool sessions (e.g. claude/dev).
    stream_idle_secs: Option<u64>,
}

#[derive(Debug, Parser)]
#[command(name = "baml-agent-runner")]
#[command(about = "Load and execute one or more packaged agents", long_about = None)]
struct Cli {
    /// Agent package tar.gz paths to load.
    #[arg(value_name = "AGENT_PACKAGE", required = true)]
    packages: Vec<PathBuf>,

    /// Invoke a JS function: <agent> <function> <json-args>
    #[arg(long, num_args = 3, value_names = ["AGENT", "FUNCTION", "JSON_ARGS"])]
    invoke: Option<Vec<String>>,

    /// Run an A2A JSON-RPC loop over stdio.
    #[arg(long)]
    a2a_stdio: bool,

    /// Bind HTTP API (discovery + A2A routing) on the given address (e.g. 127.0.0.1:8080).
    #[arg(long, value_name = "ADDR")]
    serve_http: Option<String>,

    /// Directory containing built web UI assets (e.g. web/dist).
    /// When set, the HTTP server serves these files at the root path.
    #[arg(long, value_name = "DIR")]
    web_dir: Option<PathBuf>,

    /// Provenance SQLite database path. Default is ":memory:".
    #[arg(long, value_name = "PATH", default_value = ":memory:")]
    provenance_db: String,

    /// Claude workspaces root directory (claude/dev session cwd base). When set, overrides BAML_CLAUDE_WORKSPACES_BASE. Use an absolute path or path relative to current working directory.
    #[arg(long, value_name = "DIR")]
    claude_workspaces_base: Option<PathBuf>,

    /// Stream collector idle timeout (seconds). If no chunk is yielded for this long, the stream ends with Timeout. Default 600 for long-running tool sessions (e.g. claude/dev).
    #[arg(long, value_name = "SECS", default_value = "600")]
    stream_idle_secs: u64,
}

impl Cli {
    fn into_config(self) -> anyhow::Result<RunnerConfig> {
        let invoke = self
            .invoke
            .map(|values| (values[0].clone(), values[1].clone(), values[2].clone()));

        let provenance_db = if self.provenance_db == ":memory:" {
            ProvenanceDb::InMemory
        } else {
            ProvenanceDb::File(PathBuf::from(self.provenance_db))
        };

        Ok(RunnerConfig {
            packages: self.packages,
            invoke,
            a2a_stdio: self.a2a_stdio,
            serve_http: self.serve_http,
            web_dir: self.web_dir,
            provenance_db,
            claude_workspaces_base: self.claude_workspaces_base,
            stream_idle_secs: Some(self.stream_idle_secs),
        })
    }
}

/// Provenance configuration: GraphQLite store
pub(crate) enum ProvenanceConfig {
    Graphqlite {
        store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
        mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
    },
}

impl ProvenanceConfig {
    pub(crate) fn store(&self) -> &Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
        let ProvenanceConfig::Graphqlite { store, .. } = self;
        store
    }

    pub(crate) fn mermaid_cache(&self) -> Option<Arc<baml_rt_provenance::MermaidCache>> {
        let ProvenanceConfig::Graphqlite { mermaid_cache, .. } = self;
        mermaid_cache.clone()
    }
}

fn build_provenance_config(db: &ProvenanceDb) -> Result<ProvenanceConfig> {
    match db {
        ProvenanceDb::InMemory => {
            let store = GraphqliteStoreBuilder::in_memory().build().map_err(|e| {
                BamlRtError::InvalidArgument(format!(
                    "Provenance in-memory store failed to build: {e}",
                ))
            })?;
            Ok(ProvenanceConfig::Graphqlite {
                store,
                mermaid_cache: None,
            })
        }
        ProvenanceDb::File(path) => {
            let cache = Arc::new(baml_rt_provenance::MermaidCache::new());
            let store = GraphqliteStoreBuilder::file(path)
                .with_mermaid_cache(cache.clone())
                .build()
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!(
                        "Provenance file store failed to build at {}: {:#}",
                        path.display(),
                        anyhow::Error::from(e),
                    ))
                })?;
            Ok(ProvenanceConfig::Graphqlite {
                store,
                mermaid_cache: Some(cache),
            })
        }
    }
}

/// Mermaid diagram service backed by GraphQLite provenance. Exported when runner serves HTTP with GraphQLite.
/// Uses in-process GraphExporter; GraphQLite fork has reentrant parser so no broker IPC needed.
/// Cache avoids repeated Cypher export + simplify + render on repeat requests for the same context.
struct MermaidServiceImpl {
    store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
    cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
}

impl MermaidServiceImpl {
    fn new(
        store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
        cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
    ) -> Self {
        Self { store, cache }
    }

    async fn export_by_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<baml_rt_provenance::ExportedGraph, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        exporter
            .export_by_context(context_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))
    }

    async fn export_by_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<baml_rt_provenance::ExportedGraph, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        exporter
            .export_by_task(task_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))
    }
}

#[async_trait::async_trait]
impl baml_rt_api::MermaidService for MermaidServiceImpl {
    async fn mermaid_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get(context_id) {
                tracing::debug!(context_id = %context_id, "mermaid: cache HIT");
                return Ok(cached);
            }
        }
        tracing::info!(context_id = %context_id, "mermaid: START export_by_context");
        let t0 = std::time::Instant::now();
        let graph = self.export_by_context(context_id).await?;
        tracing::info!(
            context_id = %context_id,
            export_ms = t0.elapsed().as_millis(),
            nodes = graph.nodes.len(),
            edges = graph.edges.len(),
            "mermaid: DONE export_by_context"
        );
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        tracing::info!(context_id = %context_id, "mermaid: START simplify_graph");
        let t1 = std::time::Instant::now();
        let simplified = simplify_graph(&graph);
        tracing::info!(
            context_id = %context_id,
            simplify_ms = t1.elapsed().as_millis(),
            nodes = simplified.nodes.len(),
            edges = simplified.edges.len(),
            "mermaid: DONE simplify_graph"
        );
        tracing::info!(context_id = %context_id, "mermaid: START render_sequence_diagram");
        let t2 = std::time::Instant::now();
        let mermaid = render_sequence_diagram(&simplified);
        tracing::info!(
            context_id = %context_id,
            render_ms = t2.elapsed().as_millis(),
            bytes = mermaid.len(),
            "mermaid: DONE render_sequence_diagram"
        );
        if let Some(ref cache) = self.cache {
            cache.insert(context_id, mermaid.clone());
        }
        Ok(mermaid)
    }

    async fn mermaid_for_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        tracing::info!(task_id = %task_id, "mermaid: START export_by_task");
        let t0 = std::time::Instant::now();
        let graph = self.export_by_task(task_id).await?;
        tracing::info!(
            task_id = %task_id,
            export_ms = t0.elapsed().as_millis(),
            nodes = graph.nodes.len(),
            edges = graph.edges.len(),
            "mermaid: DONE export_by_task"
        );
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        tracing::info!(task_id = %task_id, "mermaid: START simplify_graph");
        let t1 = std::time::Instant::now();
        let simplified = simplify_graph(&graph);
        tracing::info!(
            task_id = %task_id,
            simplify_ms = t1.elapsed().as_millis(),
            nodes = simplified.nodes.len(),
            edges = simplified.edges.len(),
            "mermaid: DONE simplify_graph"
        );
        tracing::info!(task_id = %task_id, "mermaid: START render_sequence_diagram");
        let t2 = std::time::Instant::now();
        let mermaid = render_sequence_diagram(&simplified);
        tracing::info!(
            task_id = %task_id,
            render_ms = t2.elapsed().as_millis(),
            bytes = mermaid.len(),
            "mermaid: DONE render_sequence_diagram"
        );
        Ok(mermaid)
    }
}

/// Context metrics service backed by GraphQLite provenance.
struct ContextMetricsServiceImpl {
    store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
}

impl ContextMetricsServiceImpl {
    fn new(store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>) -> Self {
        Self { store }
    }
}

fn metrics_query_params(context_id: &str) -> GraphQueryParams {
    let mut params = Map::new();
    params.insert(
        "context_id".to_string(),
        Value::String(context_id.to_string()),
    );
    params
}

fn value_as_u64(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_i64().map(|v| v.max(0) as u64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

fn value_as_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

#[async_trait::async_trait]
impl baml_rt_api::ContextMetricsService for ContextMetricsServiceImpl {
    async fn metrics_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<baml_rt_api::ContextMetricsResponseDto, baml_rt_api::ContextMetricsError>
    {
        let params = metrics_query_params(context_id);

        let turn_rows = self
            .store
            .query(context_metrics_queries::TURN_TOTALS_BY_CONTEXT, &params)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(e)))
            })?;

        let session_rows = self
            .store
            .query(context_metrics_queries::SESSION_TOTALS_BY_CONTEXT, &params)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(e)))
            })?;

        let prompt_rows = self
            .store
            .query(context_metrics_queries::USER_PROMPTS_BY_CONTEXT, &params)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(e)))
            })?;

        let mut prompt_count_by_message: HashMap<String, u64> = HashMap::new();
        for row in prompt_rows {
            let message_id = value_as_string(row.get("message_id"));
            if message_id.is_empty() {
                continue;
            }
            prompt_count_by_message.insert(message_id, value_as_u64(row.get("user_prompt_count")));
        }

        let mut turns = Vec::with_capacity(turn_rows.len());
        for row in turn_rows {
            let message_id = value_as_string(row.get("message_id"));
            if message_id.is_empty() {
                continue;
            }
            let user_prompt_count = prompt_count_by_message.remove(&message_id).unwrap_or(0);
            turns.push(baml_rt_api::ContextTurnMetricsDto {
                message_id,
                user_prompt_count,
                llm_call_count: value_as_u64(row.get("llm_call_count")),
                llm_duration_ms_total: value_as_u64(row.get("llm_duration_ms_total")),
                tokens: baml_rt_api::TokenUsageDto {
                    input: value_as_u64(row.get("tokens_in")),
                    output: value_as_u64(row.get("tokens_out")),
                    total: value_as_u64(row.get("tokens_total")),
                },
            });
        }

        // Keep prompt-only turns visible even when no LLM call was made.
        let mut prompt_only_turns = prompt_count_by_message.into_iter().collect::<Vec<_>>();
        prompt_only_turns.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (message_id, user_prompt_count) in prompt_only_turns {
            turns.push(baml_rt_api::ContextTurnMetricsDto {
                message_id,
                user_prompt_count,
                llm_call_count: 0,
                llm_duration_ms_total: 0,
                tokens: baml_rt_api::TokenUsageDto {
                    input: 0,
                    output: 0,
                    total: 0,
                },
            });
        }

        let session = session_rows.first();
        let session_tokens_in = value_as_u64(session.and_then(|row| row.get("tokens_in")));
        let session_tokens_out = value_as_u64(session.and_then(|row| row.get("tokens_out")));
        let session_tokens_total = value_as_u64(session.and_then(|row| row.get("tokens_total")));
        let session_llm_calls = value_as_u64(session.and_then(|row| row.get("llm_call_count")));
        let session_llm_duration_ms =
            value_as_u64(session.and_then(|row| row.get("llm_duration_ms_total")));

        let user_prompts_total = turns.iter().map(|turn| turn.user_prompt_count).sum();
        let turns_total = turns.len() as u64;

        Ok(baml_rt_api::ContextMetricsResponseDto {
            context_id: context_id.to_string(),
            turns,
            session: baml_rt_api::ContextSessionMetricsDto {
                turns_total,
                user_prompts_total,
                llm_calls_total: session_llm_calls,
                llm_duration_ms_total: session_llm_duration_ms,
                tokens_total: baml_rt_api::TokenUsageDto {
                    input: session_tokens_in,
                    output: session_tokens_out,
                    total: session_tokens_total,
                },
            },
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_setup::init_tracing();
    match dotenvy::dotenv() {
        Ok(path) => tracing::debug!(path = ?path, "Loaded .env"),
        Err(err) => tracing::debug!(error = ?err, "No .env loaded"),
    }

    info!("BAML Agent Runner starting");

    // Parse command line arguments
    let config = Cli::parse()
        .into_config()
        .context("Failed to parse arguments")?;

    // If --claude-workspaces-base is set, resolve to absolute path and set env so package boot uses it.
    if let Some(ref base) = config.claude_workspaces_base {
        let absolute = if base.is_absolute() {
            base.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(base)
        };
        if let Err(e) = std::fs::create_dir_all(&absolute) {
            eprintln!(
                "Error: Cannot create Claude workspaces base {}: {}",
                absolute.display(),
                e
            );
            std::process::exit(1);
        }
        let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
        // SAFETY: single-threaded at this point; no other thread reads this var before we load packages.
        unsafe {
            std::env::set_var(
                "BAML_CLAUDE_WORKSPACES_BASE",
                canonical.to_string_lossy().to_string(),
            );
        }
        info!(
            base = %canonical.display(),
            "Claude workspaces base set from --claude-workspaces-base (overrides env)",
        );
    }

    match &config.provenance_db {
        ProvenanceDb::InMemory => info!(
            "Provenance backend: in-memory (:memory:). External graph_exporter cannot read this process-local data."
        ),
        ProvenanceDb::File(path) => {
            info!(path = %path.display(), "Provenance backend: sqlite file")
        }
    }
    let provenance_config = build_provenance_config(&config.provenance_db)
        .context("Failed to initialize provenance storage")?;
    let access_allowlist = parse_access_allowlist();
    let tool_index = match &config.provenance_db {
        ProvenanceDb::InMemory => Some(ToolIndexConfig::in_memory()),
        ProvenanceDb::File(path) => Some(ToolIndexConfig::new(path)),
    };
    let mut builder = builder::RunnerBuilder::<builder::Loading>::new(
        provenance_config,
        tool_index,
        access_allowlist,
        config.stream_idle_secs,
    );

    for package in &config.packages {
        let package_path = Path::new(package);
        if !package_path.exists() {
            eprintln!("Error: Agent package not found: {}", package_path.display());
            std::process::exit(1);
        }

        match builder.load_agent(package_path).await {
            Ok(b) => {
                builder = b;
                info!(package_path = %package_path.display(), "Agent package loaded");
            }
            Err(e) => {
                error!(error = %e, package = %package_path.display(), "Failed to load agent package");
                eprintln!(
                    "Error: Failed to load agent package {}: {}",
                    package_path.display(),
                    e
                );
                std::process::exit(1);
            }
        }
    }

    let ready = builder.build();

    if let Some((agent_name, function_name, json_args)) = config.invoke {
        let args_value: Value =
            serde_json::from_str(&json_args).context("Invalid JSON arguments")?;
        let result = ready
            .invoke(&agent_name, &function_name, args_value)
            .await
            .context("Function invocation failed")?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let agents = ready.list_agents();
    if agents.is_empty() {
        eprintln!("Error: No agents loaded");
        std::process::exit(1);
    }

    println!("✅ Loaded {} agent(s):", agents.len());
    for agent_name in &agents {
        println!("  - {}", agent_name);
    }

    let http_handle = if let Some(bind) = config.serve_http.clone() {
        let (mermaid, context_metrics) = {
            let config = ready.runner().provenance_config();
            let store = config.store().clone();
            (
                Some(Arc::new(MermaidServiceImpl::new(
                    store.clone(),
                    config.mermaid_cache(),
                )) as Arc<dyn baml_rt_api::MermaidService>),
                Some(Arc::new(ContextMetricsServiceImpl::new(store))
                    as Arc<dyn baml_rt_api::ContextMetricsService>),
            )
        };
        let registry_impl = ready.registry();
        let web_dir = config.web_dir.clone();
        info!(
            bind = %bind,
            web_dir = ?web_dir,
            "A2A server mode: exposing HTTP API (GET /agents, POST /agents/.../a2a/sse, GET /contexts/.../mermaid, GET /tasks/.../mermaid, GET /contexts/.../metrics, GET /openapi.json)"
        );
        Some(tokio::spawn(async move {
            baml_rt_api::serve_with_services(
                registry_impl,
                &bind,
                mermaid,
                context_metrics,
                web_dir.as_deref(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("HTTP API server: {e}"))
        }))
    } else {
        None
    };

    match (config.a2a_stdio, http_handle) {
        (true, Some(mut handle)) => {
            let stdio_fut = ready.run_a2a_stdio();
            tokio::pin!(stdio_fut);
            let mut http_exited = false;

            loop {
                tokio::select! {
                    stdio_result = &mut stdio_fut => {
                        if !http_exited && !handle.is_finished() {
                            info!("A2A stdio loop ended; stopping HTTP API server task");
                            handle.abort();
                        }
                        stdio_result?;
                        break;
                    }
                    http_result = &mut handle, if !http_exited => {
                        match http_result {
                            Ok(Ok(())) => warn!("HTTP API server exited; continuing A2A stdio loop"),
                            Ok(Err(err)) => warn!(error = %err, "HTTP API server exited with error; continuing A2A stdio loop"),
                            Err(join_err) if join_err.is_cancelled() => info!("HTTP API server task was cancelled; continuing A2A stdio loop"),
                            Err(join_err) => warn!("HTTP API server task join error: {join_err}; continuing A2A stdio loop"),
                        }
                        http_exited = true;
                    }
                }
            }
        }
        (true, None) => {
            ready.run_a2a_stdio().await?;
        }
        (false, Some(handle)) => {
            handle.await??;
            return Ok(());
        }
        (false, None) => {}
    }

    info!("Agent Runner completed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt_core::{bus::BusWithEffects, route_key_from_request};
    use serde_json::json;

    use super::*;

    fn test_provenance_config() -> ProvenanceConfig {
        let store = GraphqliteStoreBuilder::in_memory()
            .build()
            .expect("in-memory provenance store for test");
        ProvenanceConfig::Graphqlite {
            store,
            mermaid_cache: None,
        }
    }

    async fn build_test_agent() -> A2aAgent {
        let manager = BamlRuntimeManager::new().expect("create runtime manager");
        let store = GraphqliteStoreBuilder::in_memory()
            .build()
            .expect("in-memory store for test agent");
        let code = r#"
globalThis.onChatMessage = async function(_message) {
  __chat_yield({ message: { parts: [{ text: "ok" }] } });
  __chat_yield({ final: true });
};
"#;
        A2aAgent::builder()
            .with_runtime_manager(manager)
            .with_init_js(code)
            .with_effect_emitter(Arc::new(BusWithEffects::new()))
            .with_graphqlite_store(store)
            .build()
            .await
            .expect("build test agent")
    }

    async fn insert_test_agent(runner: &AgentRunner, package_name: &str) {
        let package = AgentPackageName::parse(package_name).expect("valid package name");
        let route_key = AgentRouteKey::new(package.clone(), AgentInstanceId::default());
        let manifest = AgentManifest {
            name: package_name.to_string(),
            version: "1.0.0".to_string(),
            entry_point: "dist/index.js".to_string(),
            signature: format!("{package_name}@1.0.0"),
            tools: vec![],
            discovery: None,
        };
        runner.insert_agent(
            package_name.to_string(),
            route_key,
            BootedAgent {
                agent: build_test_agent().await,
                manifest,
            },
        );
    }

    #[tokio::test]
    async fn prepare_a2a_request_defaults_to_coordinator_for_plaintext_with_multiple_agents() {
        let runner = AgentRunner::new(
            test_provenance_config(),
            None,
            ToolAccessPolicy::default(),
            None,
        );
        insert_test_agent(&runner, "coordinator-agent").await;
        insert_test_agent(&runner, "notion-agent").await;
        insert_test_agent(&runner, "clickup-agent").await;

        let mut request =
            wrap_plaintext_message("in clickup agent, what are my tasks in progress?")
                .expect("wrap plaintext request");
        let (agent_name, prepared) = runner
            .prepare_a2a_request(&mut request)
            .expect("coordinator implicit routing");

        assert_eq!(agent_name, "coordinator-agent");
        assert_eq!(
            prepared
                .get("method")
                .and_then(Value::as_str)
                .expect("method"),
            "message.sendStream"
        );
        assert_eq!(
            prepared
                .get("params")
                .and_then(|params| params.get("message"))
                .and_then(|message| message.get("metadata"))
                .and_then(|metadata| metadata.get("agent"))
                .and_then(Value::as_str)
                .expect("message metadata agent"),
            "coordinator-agent"
        );
    }

    #[tokio::test]
    async fn prepare_a2a_request_still_errors_without_coordinator_when_multiple_agents_loaded() {
        let runner = AgentRunner::new(
            test_provenance_config(),
            None,
            ToolAccessPolicy::default(),
            None,
        );
        insert_test_agent(&runner, "notion-agent").await;
        insert_test_agent(&runner, "clickup-agent").await;

        let mut request = wrap_plaintext_message("list tasks").expect("wrap plaintext request");
        let err = runner
            .prepare_a2a_request(&mut request)
            .expect_err("missing explicit agent should still fail");

        assert!(
            err.to_string().contains("A2A request missing agent"),
            "expected missing-agent error, got: {err}"
        );
    }

    #[tokio::test]
    async fn internal_a2a_router_rejects_self_routing_by_route_key() {
        let runner = Arc::new(AgentRunner::new(
            test_provenance_config(),
            None,
            ToolAccessPolicy::default(),
            None,
        ));
        runner.internal_a2a_router().set_runner(runner.clone());
        let caller = AgentRouteKey::new(
            AgentPackageName::parse("coordinator-agent").expect("valid caller package"),
            AgentInstanceId::default(),
        );
        let request = json!({
            "jsonrpc": "2.0",
            "method": "message.sendStream",
            "params": {
                "metadata": {
                    "target": {
                        "agent_package": "coordinator-agent",
                        "agent_instance_id": "default"
                    }
                }
            },
            "id": null
        });

        let err = match runner
            .internal_a2a_router()
            .route_from(&caller, baml_rt_core::A2aWireRequest::from(request))
            .await
        {
            Ok(_) => panic!("self-route must be rejected"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("self-routing"),
            "expected self-routing guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn handle_a2a_by_key_respects_instance_id() {
        let runner = AgentRunner::new(
            test_provenance_config(),
            None,
            ToolAccessPolicy::default(),
            None,
        );
        let package_name = AgentPackageName::parse("demo-agent").expect("valid package");
        let default_key = AgentRouteKey::new(package_name.clone(), AgentInstanceId::default());
        let staging_key = AgentRouteKey::new(
            package_name.clone(),
            AgentInstanceId::parse("staging").expect("valid instance"),
        );

        let agent = build_test_agent().await;
        let manifest = AgentManifest {
            name: package_name.as_str().to_string(),
            version: "1.0.0".to_string(),
            entry_point: "dist/index.js".to_string(),
            signature: "demo-agent@1.0.0".to_string(),
            tools: vec![],
            discovery: None,
        };
        runner.insert_agent(
            package_name.as_str().to_string(),
            default_key,
            BootedAgent { agent, manifest },
        );

        let err = match runner
            .handle_a2a_by_key(
                &staging_key,
                A2aWireRequest::from(json!({
                    "jsonrpc": "2.0",
                    "method": "message.sendStream",
                    "params": {"message": {"parts": [{"text": "ping"}]}}
                })),
            )
            .await
        {
            Ok(_) => panic!("non-default instance should not route to default agent"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("demo-agent/staging not found"),
            "expected instance-specific not found error, got: {err}"
        );
    }

    #[test]
    fn route_key_from_request_extracts_key() {
        let request = json!({
            "jsonrpc": "2.0",
            "method": "message.sendStream",
            "params": {
                "metadata": {
                    "target": {
                        "agent_package": "my-pkg",
                        "agent_instance_id": "inst-1"
                    }
                }
            },
            "id": 1
        });
        let key = route_key_from_request(baml_rt_core::A2aWireRequest::from(request)).unwrap();
        assert_eq!(key.agent_package.as_str(), "my-pkg");
        assert_eq!(key.agent_instance_id.as_str(), "inst-1");
    }

    #[test]
    fn route_key_from_request_default_instance_id() {
        let request = json!({
            "params": {
                "metadata": {
                    "target": {
                        "agent_package": "solo"
                    }
                }
            }
        });
        let key = route_key_from_request(baml_rt_core::A2aWireRequest::from(request)).unwrap();
        assert_eq!(key.agent_package.as_str(), "solo");
        assert_eq!(key.agent_instance_id.as_str(), "default");
    }

    #[test]
    fn route_key_from_request_missing_target_err() {
        let request = json!({
            "params": { "metadata": {} }
        });
        assert!(route_key_from_request(baml_rt_core::A2aWireRequest::from(request)).is_err());
    }
}
