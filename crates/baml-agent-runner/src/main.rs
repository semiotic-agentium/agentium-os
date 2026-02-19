//! BAML Agent Runner
//!
//! This binary loads and executes one or more packaged agent applications.
//! Each agent package is a tar.gz containing BAML schemas, compiled TypeScript,
//! and metadata.

#![recursion_limit = "256"]

mod builder;
mod package;

use anyhow::Context;
use async_trait::async_trait;
use baml_rt_a2a::a2a_types::A2aMessageId;
use baml_rt_a2a::a2a_types::{
    JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, ROLE_USER, SendMessageConfiguration,
    SendMessageRequest,
};
use baml_rt_a2a::{A2aAgent, A2aRequestHandler, AgentRegistry, a2a};
use baml_rt_core::bus::BusStream;
use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, DerivedId, ExternalId, TaskId};
use baml_rt_core::{
    AgentCard, AgentDiscoveryEntry, AgentLister, AgentManifest, AgentRouteKey, BamlRtError,
    ContextId, Result, collect_a2a_stream, route_key_from_request,
};
use baml_rt_observability::{spans, tracing_setup};
use baml_rt_provenance::ProvenanceWriter;
use baml_rt_provenance::graph_export::sequence::render_sequence_diagram;
use baml_rt_provenance::graph_export::simplify::simplify_graph;
use baml_rt_provenance::{
    AgentType, GraphExporter, GraphqliteStoreBuilder, ProvEvent, ToolIndexConfig, index_tools,
};
use baml_rt_quickjs::BamlRuntimeManager;
use baml_rt_tools::{
    ManifestToolNames, ToolAccessPolicy, parse_access_allowlist, register_manifest_tools,
};
use baml_rt_tools_system::SystemBundle;
use clap::Parser;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Inert agent package - just holds package data
pub(crate) struct AgentPackage {
    manifest: AgentManifest,
    extract_dir: PathBuf,
    baml_src: PathBuf,
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
    ) -> Result<(A2aAgent, AgentId)> {
        let span = spans::load_agent_package(&self.extract_dir);
        let _guard = span.enter();

        // Create runtime manager and load BAML schema
        let mut runtime_manager = BamlRuntimeManager::new()?;
        {
            let schema_span = spans::load_baml_schema(&self.baml_src);
            let _schema_guard = schema_span.enter();
            let baml_src_str = self.baml_src.to_str().ok_or_else(|| {
                BamlRtError::InvalidArgument("BAML source path contains invalid UTF-8".to_string())
            })?;
            runtime_manager.load_schema(baml_src_str)?;
            info!(agent = self.manifest.name, "BAML schema loaded");
        }

        runtime_manager
            .set_tool_allowlist(self.manifest.tools.iter().cloned().collect::<HashSet<_>>())
            .await?;

        let manifest_tool_names = ManifestToolNames::parse(&self.manifest.tools)?;
        register_manifest_tools(
            runtime_manager.tool_registry().as_ref(),
            &manifest_tool_names,
            policy,
        )?;

        // Host composes tool catalogue: system bundle (internal_a2a, discover_agents, discover_tools)
        let runtime_manager_arc = Arc::new(Mutex::new(runtime_manager));
        let tool_registry = runtime_manager_arc.lock().await.tool_registry();
        tool_registry.register_bundle(SystemBundle::new(
            agent_list_catalogue,
            tool_registry.clone(),
            a2a_handler,
        ))?;

        let mut agent_builder = A2aAgent::builder()
            .with_runtime_handle(runtime_manager_arc.clone())
            .with_baml_helpers(true) // Register BAML functions
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()));

        match provenance_config {
            ProvenanceConfig::Graphqlite(store) => {
                agent_builder = agent_builder.with_graphqlite_store(store.clone());
            }
            ProvenanceConfig::None => {}
        }

        let agent = agent_builder.build().await?;

        // Load and evaluate agent JavaScript code
        let entry_point_path = self.extract_dir.join(&self.manifest.entry_point);
        if entry_point_path.exists() {
            let eval_span = spans::evaluate_agent_code(&self.manifest.entry_point);
            let _eval_guard = eval_span.enter();

            let agent_code = std::fs::read_to_string(&entry_point_path).map_err(BamlRtError::Io)?;

            info!(
                entry_point = self.manifest.entry_point,
                "Loading agent JavaScript code"
            );

            let bridge = agent.bridge();
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

        // Emit AgentBooted provenance event
        let writer: Option<Arc<dyn ProvenanceWriter>> = match provenance_config {
            ProvenanceConfig::Graphqlite(store) => Some(store.clone() as Arc<dyn ProvenanceWriter>),
            ProvenanceConfig::None => None,
        };
        if let Some(writer) = writer {
            // Use stable archive identity from manifest signature
            let archive_path = self.manifest.signature.clone();
            let context_id = context::generate_context_id();
            let agent_type_parsed =
                AgentType::new(self.manifest.name.clone()).ok_or_else(|| {
                    BamlRtError::InvalidArgument("agent_type cannot be empty".to_string())
                })?;
            let boot_event = ProvEvent::agent_booted(
                context_id,
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

    async fn handle_a2a_stream(&self, request: Value) -> Result<BusStream<Value>> {
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
}

impl AgentRunner {
    pub(crate) fn new(
        provenance_config: ProvenanceConfig,
        tool_index: Option<ToolIndexConfig>,
        access_policy: ToolAccessPolicy,
    ) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            provenance_config,
            tool_index,
            access_policy,
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

    /// Insert a booted agent (used by builder during load phase).
    pub(crate) fn insert_agent(&self, name: String, booted: BootedAgent) {
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

        let agents = self.agents.read().expect("RwLock poison");
        let agent = agents.get(agent_name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!("Agent '{}' not found", agent_name))
        })?;
        let agent = agent.clone();
        drop(agents);
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
                    agent_instance_id: "default".to_string(),
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
                    agent_instance_id: "default".to_string(),
                    name: m.name.clone(),
                    version,
                    agent_card,
                }
            })
            .collect()
    }

    /// Handle A2A request by route key (for HTTP POST /agents/.../a2a).
    pub(crate) async fn handle_a2a_by_key(
        &self,
        key: &AgentRouteKey,
        request: Value,
    ) -> Result<BusStream<Value>> {
        info!(
            agent_package = %key.agent_package,
            agent_instance_id = %key.agent_instance_id,
            "A2A request: dispatching to agent"
        );
        let booted = self
            .agents
            .read()
            .expect("RwLock poison")
            .get(&key.agent_package)
            .cloned()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "Agent {}/{} not found",
                    key.agent_package, key.agent_instance_id
                ))
            })?;
        let scope = InvocationScope::synthetic_message(booted.agent.agent_id().clone());
        let agent = booted.agent.clone();
        context::with_scope(scope.as_scope().clone(), async move {
            agent.handle_a2a_stream(request).await
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
                Ok(_) => wrap_plaintext_message(line),
                Err(_) => wrap_plaintext_message(line),
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

            let agents = self.agents.read().expect("RwLock poison");
            let agent = match agents.get(&agent_name) {
                Some(agent) => agent.clone(),
                None => {
                    drop(agents);
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
                }
            };
            drop(agents);

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

            let responses = match agent.handle_a2a_stream(prepared_request).await {
                Ok(stream) => collect_a2a_stream(stream).await,
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
            if agents.len() == 1 {
                let agent_name = agents.keys().next().cloned().unwrap_or_default();
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
        } else if let Some((agent_name, method_name)) = split_agent_method(&method_base, &*agents) {
            (agent_name, method_name)
        } else if agents.len() == 1 {
            let agent_name = agents.keys().next().cloned().unwrap_or_default();
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
        request: Value,
    ) -> Result<BusStream<Value>> {
        self.0.handle_a2a_by_key(key, request).await
    }
}

#[async_trait]
impl A2aRequestHandler for RunnerRegistry {
    async fn handle_a2a_stream(&self, request: Value) -> Result<BusStream<Value>> {
        let key = route_key_from_request(&request)?;
        self.0.handle_a2a_by_key(&key, request).await
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

fn is_a2a_method(method: &str) -> bool {
    method.starts_with("message/") || method.starts_with("tasks/") || method.starts_with("agent/")
}

fn map_a2a_error(id: Option<JSONRPCId>, err: BamlRtError) -> Value {
    match err {
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
static CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(1);
static STDIO_CONTEXT_ID: std::sync::OnceLock<ContextId> = std::sync::OnceLock::new();
static STDIO_TASK_ID: std::sync::OnceLock<TaskId> = std::sync::OnceLock::new();

fn stdio_context_id() -> ContextId {
    STDIO_CONTEXT_ID
        .get_or_init(|| {
            let _ = CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
            context::generate_context_id()
        })
        .clone()
}

fn stdio_task_id() -> TaskId {
    STDIO_TASK_ID
        .get_or_init(|| {
            TaskId::from_external(ExternalId::new(format!(
                "cli-task-{}",
                stdio_context_id().as_str()
            )))
        })
        .clone()
}

fn wrap_plaintext_message(text: &str) -> Value {
    let message_id = A2aMessageId::outgoing(DerivedId::new(format!(
        "cli-msg-{}",
        MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )));
    let message = Message {
        message_id,
        role: MessageRole::String(ROLE_USER.to_string()),
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
    serde_json::to_value(request).unwrap_or(Value::Null)
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
        })
    }
}

/// Provenance configuration: none, writer only, or GraphQLite (store required).
pub(crate) enum ProvenanceConfig {
    None,
    Graphqlite(Arc<baml_rt_provenance::GraphqliteProvenanceStore>),
}

fn build_provenance_config(db: &ProvenanceDb) -> ProvenanceConfig {
    let arc = match db {
        ProvenanceDb::InMemory => match GraphqliteStoreBuilder::in_memory().build() {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "Provenance in-memory store failed to build");
                return ProvenanceConfig::None;
            }
        },
        ProvenanceDb::File(path) => match GraphqliteStoreBuilder::file(path).build() {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Provenance file store failed to build");
                return ProvenanceConfig::None;
            }
        },
    };
    ProvenanceConfig::Graphqlite(arc)
}

/// Mermaid diagram service backed by GraphQLite provenance. Exported when runner serves HTTP with GraphQLite.
struct MermaidServiceImpl {
    store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
}

impl MermaidServiceImpl {
    fn new(store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl baml_rt_api::MermaidService for MermaidServiceImpl {
    async fn mermaid_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        let graph = exporter
            .export_by_context(context_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let simplified = simplify_graph(&graph);
        Ok(render_sequence_diagram(&simplified))
    }

    async fn mermaid_for_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        let graph = exporter
            .export_by_task(task_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let simplified = simplify_graph(&graph);
        Ok(render_sequence_diagram(&simplified))
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
    match &config.provenance_db {
        ProvenanceDb::InMemory => info!(
            "Provenance backend: in-memory (:memory:). External graph_exporter cannot read this process-local data."
        ),
        ProvenanceDb::File(path) => {
            info!(path = %path.display(), "Provenance backend: sqlite file")
        }
    }
    let provenance_config = build_provenance_config(&config.provenance_db);
    let access_allowlist = parse_access_allowlist();
    let tool_index = match &config.provenance_db {
        ProvenanceDb::InMemory => Some(ToolIndexConfig::in_memory()),
        ProvenanceDb::File(path) => Some(ToolIndexConfig::new(path)),
    };
    let mut builder = builder::RunnerBuilder::<builder::Loading>::new(
        provenance_config,
        tool_index,
        access_allowlist,
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

    if let Some(bind) = &config.serve_http {
        let mermaid = match ready.runner().provenance_config() {
            ProvenanceConfig::Graphqlite(store) => {
                Some(Arc::new(MermaidServiceImpl::new(store.clone()))
                    as Arc<dyn baml_rt_api::MermaidService>)
            }
            _ => None,
        };
        let registry_impl = ready.registry();
        let web_dir = config.web_dir.as_deref();
        info!(bind = %bind, web_dir = ?web_dir, "A2A server mode: exposing HTTP API (GET /agents, POST /agents/.../a2a, GET /mermaid/..., GET /openapi.json)");
        baml_rt_api::serve(registry_impl, bind, mermaid, web_dir)
            .await
            .map_err(|e| anyhow::anyhow!("HTTP API server: {e}"))?;
        return Ok(());
    }

    if config.a2a_stdio {
        ready.run_a2a_stdio().await?;
        return Ok(());
    }

    info!("Agent Runner completed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use baml_rt_core::route_key_from_request;

    #[test]
    fn route_key_from_request_extracts_key() {
        let request = serde_json::json!({
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
        let key = route_key_from_request(&request).unwrap();
        assert_eq!(key.agent_package, "my-pkg");
        assert_eq!(key.agent_instance_id, "inst-1");
    }

    #[test]
    fn route_key_from_request_default_instance_id() {
        let request = serde_json::json!({
            "params": {
                "metadata": {
                    "target": {
                        "agent_package": "solo"
                    }
                }
            }
        });
        let key = route_key_from_request(&request).unwrap();
        assert_eq!(key.agent_package, "solo");
        assert_eq!(key.agent_instance_id, "default");
    }

    #[test]
    fn route_key_from_request_missing_target_err() {
        let request = serde_json::json!({
            "params": { "metadata": {} }
        });
        assert!(route_key_from_request(&request).is_err());
    }
}
