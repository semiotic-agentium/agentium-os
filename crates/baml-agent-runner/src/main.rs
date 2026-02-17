//! BAML Agent Runner
//!
//! This binary loads and executes one or more packaged agent applications.
//! Each agent package is a tar.gz containing BAML schemas, compiled TypeScript,
//! and metadata.

#![recursion_limit = "256"]

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
    AgentDiscoveryEntry, AgentManifest, AgentRouteKey, BamlRtError, ContextId, Result,
    collect_a2a_stream,
};
use baml_rt_observability::{spans, tracing_setup};
use baml_rt_provenance::{AgentType, ProvEvent, ToolIndexConfig, index_tools};
use baml_rt_provenance::{FalkorDbProvenanceConfig, FalkorDbProvenanceWriter, ProvenanceWriter};
use baml_rt_quickjs::BamlRuntimeManager;
use baml_rt_tools::tools::ToolAccess;
use baml_rt_tools::{enforce_tool_access, parse_access_allowlist};
use clap::{Parser, ValueEnum};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Inert agent package - just holds package data
struct AgentPackage {
    manifest: AgentManifest,
    extract_dir: PathBuf,
    baml_src: PathBuf,
}

impl AgentPackage {
    /// Load an agent package from a tar.gz file (inert - does not boot the agent)
    async fn load_from_file(package_path: &Path) -> Result<Self> {
        let (extract_dir, manifest) = package::load_package(package_path).await?;
        let baml_src = extract_dir.join("baml_src");
        Ok(Self {
            manifest,
            extract_dir,
            baml_src,
        })
    }

    /// Boot this package into a running A2aAgent
    ///
    /// This creates the runtime, loads BAML schema, creates QuickJS bridge,
    /// loads JavaScript code, and returns a configured A2aAgent.
    /// The agent_id is generated internally by A2aAgent.
    async fn boot(
        &self,
        provenance_writer: Option<Arc<dyn ProvenanceWriter>>,
        tool_index: Option<ToolIndexConfig>,
        access_allowlist: &Option<HashSet<ToolAccess>>,
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

        // Register tool instances for every tool declared in the manifest
        for tool_name in &self.manifest.tools {
            enforce_tool_access(tool_name, access_allowlist)?;
            match tool_name.as_str() {
                "support/calculate" => {
                    runtime_manager
                        .register_tool(baml_rt_tools::support::CalculatorTool)
                        .await?;
                }
                "support/clickup" => {
                    #[cfg(feature = "clickup")]
                    {
                        runtime_manager
                            .register_tool(baml_rt_tools::clickup::ClickUpTool::new())
                            .await?;
                    }
                    #[cfg(not(feature = "clickup"))]
                    {
                        return Err(BamlRtError::InvalidArgument(
                            "ClickUp tool not compiled: enable baml-agent-runner feature 'clickup'"
                                .to_string(),
                        ));
                    }
                }
                "support/notion" => {
                    #[cfg(feature = "notion")]
                    {
                        runtime_manager
                            .register_tool(baml_rt_tools::notion::NotionTool::new())
                            .await?;
                    }
                    #[cfg(not(feature = "notion"))]
                    {
                        return Err(BamlRtError::InvalidArgument(
                            "Notion tool not compiled: enable baml-agent-runner feature 'notion'"
                                .to_string(),
                        ));
                    }
                }
                "support/notionSearchPages" => {
                    #[cfg(feature = "notion")]
                    {
                        runtime_manager
                            .register_tool(baml_rt_tools::notion::NotionSearchPagesTool::new())
                            .await?;
                    }
                    #[cfg(not(feature = "notion"))]
                    {
                        return Err(BamlRtError::InvalidArgument(
                            "Notion tool not compiled: enable baml-agent-runner feature 'notion'"
                                .to_string(),
                        ));
                    }
                }
                "support/notionGetPage" => {
                    #[cfg(feature = "notion")]
                    {
                        runtime_manager
                            .register_tool(baml_rt_tools::notion::NotionGetPageTool::new())
                            .await?;
                    }
                    #[cfg(not(feature = "notion"))]
                    {
                        return Err(BamlRtError::InvalidArgument(
                            "Notion tool not compiled: enable baml-agent-runner feature 'notion'"
                                .to_string(),
                        ));
                    }
                }
                "support/notionGetPageBlocks" => {
                    #[cfg(feature = "notion")]
                    {
                        runtime_manager
                            .register_tool(baml_rt_tools::notion::NotionGetPageBlocksTool::new())
                            .await?;
                    }
                    #[cfg(not(feature = "notion"))]
                    {
                        return Err(BamlRtError::InvalidArgument(
                            "Notion tool not compiled: enable baml-agent-runner feature 'notion'"
                                .to_string(),
                        ));
                    }
                }
                "system/internal_a2a" => {
                    // Registered by A2aAgent at build time when with_a2a_session_tool(true)
                }
                other => {
                    warn!(
                        tool = other,
                        "Unknown tool in manifest, skipping registration"
                    );
                }
            }
        }

        // Build A2aAgent - it will generate agent_id internally and create QuickJS bridge
        let runtime_manager_arc = Arc::new(Mutex::new(runtime_manager));
        let wants_a2a_session = self
            .manifest
            .tools
            .iter()
            .any(|t| t == "system/internal_a2a");
        let mut agent_builder = A2aAgent::builder()
            .with_runtime_handle(runtime_manager_arc.clone())
            .with_baml_helpers(true) // Register BAML functions
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_a2a_session_tool(wants_a2a_session);

        if let Some(writer) = provenance_writer.clone() {
            agent_builder = agent_builder.with_provenance_writer(writer);
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
                warn!(error = %err, "Failed to index tool metadata in FalkorDB");
            } else {
                info!("Tool metadata indexed in FalkorDB");
            }
        }

        // Get agent_id from the agent (generated during A2aAgent::build())
        let agent_id = agent.agent_id().clone();

        // Emit AgentBooted provenance event
        if let Some(writer) = provenance_writer {
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
                self.manifest.version.clone(),
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

    /// Get the manifest version
    fn version(&self) -> &str {
        &self.manifest.version
    }
}

/// Booted agent - holds the running A2aAgent and metadata for discovery.
struct BootedAgent {
    agent: A2aAgent,
    name: String,
    version: String,
}

impl BootedAgent {
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

/// Agent runner that manages multiple agent packages
struct AgentRunner {
    agents: HashMap<String, BootedAgent>,
    provenance_writer: Option<Arc<dyn ProvenanceWriter>>,
    tool_index: Option<ToolIndexConfig>,
    access_allowlist: Option<HashSet<ToolAccess>>,
}

impl AgentRunner {
    fn new(
        provenance_writer: Option<Arc<dyn ProvenanceWriter>>,
        tool_index: Option<ToolIndexConfig>,
        access_allowlist: Option<HashSet<ToolAccess>>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            provenance_writer,
            tool_index,
            access_allowlist,
        }
    }

    /// Load and boot an agent package
    async fn load_agent(&mut self, package_path: &Path) -> Result<()> {
        let package = AgentPackage::load_from_file(package_path).await?;
        let name = package.name().to_string();
        // Boot the package into a running agent
        let (agent, _agent_id) = package
            .boot(
                self.provenance_writer.clone(),
                self.tool_index.clone(),
                &self.access_allowlist,
            )
            .await?;

        let version = package.version().to_string();
        let booted = BootedAgent {
            agent,
            name: name.clone(),
            version: version.clone(),
        };

        info!(agent = name, "Agent loaded and booted successfully");
        self.agents.insert(name.clone(), booted);
        Ok(())
    }

    /// Execute a function in a specific agent
    async fn invoke(&self, agent_name: &str, function_name: &str, args: Value) -> Result<Value> {
        let span = spans::invoke_function(None, agent_name, function_name);
        let _guard = span.enter();

        let agent = self.agents.get(agent_name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!("Agent '{}' not found", agent_name))
        })?;

        agent.invoke_function(function_name, args).await
    }

    /// List loaded agent names (for CLI display).
    fn list_agents(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    /// List running agents as discovery entries (for HTTP GET /agents).
    fn discovery_entries(&self) -> Vec<AgentDiscoveryEntry> {
        self.agents
            .iter()
            .map(|(pkg, booted)| AgentDiscoveryEntry {
                agent_package: pkg.clone(),
                agent_instance_id: "default".to_string(),
                name: booted.name.clone(),
                version: booted.version.clone(),
            })
            .collect()
    }

    /// Handle A2A request by route key (for HTTP POST /agents/.../a2a).
    async fn handle_a2a_by_key(
        &self,
        key: &AgentRouteKey,
        request: Value,
    ) -> Result<BusStream<Value>> {
        let booted = self.agents.get(&key.agent_package).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "Agent {}/{} not found",
                key.agent_package, key.agent_instance_id
            ))
        })?;
        let scope = InvocationScope::synthetic_message(booted.agent.agent_id().clone());
        let agent = booted.agent.clone();
        // NOTE: SSE streams must stay on the host runtime. A short-lived runtime
        // would drop spawned tasks inside the stream handler, resulting in only
        // keep-alives on the client.
        context::with_scope(scope.as_scope().clone(), async move {
            agent.handle_a2a_stream(request).await
        })
        .await
    }

    /// Run the A2A JSON-RPC loop over the given reader/writer (one JSON-RPC request per line).
    /// Enables tests to use in-memory buffers instead of stdin/stdout.
    async fn run_a2a_loop<R, W>(&self, reader: R, mut writer: W) -> Result<()>
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

            let agent = match self.agents.get(&agent_name) {
                Some(agent) => agent,
                None => {
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

    async fn run_a2a_stdio(&self) -> Result<()> {
        use tokio::io;
        let stdin = io::stdin();
        let stdout = io::stdout();
        self.run_a2a_loop(io::BufReader::new(stdin), stdout).await
    }

    fn prepare_a2a_request(&self, request: &mut Value) -> Result<(String, Value)> {
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BamlRtError::InvalidArgument("A2A request missing method".to_string()))?
            .to_string();

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
            if self.agents.len() == 1 {
                let agent_name = self.agents.keys().next().cloned().unwrap_or_default();
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
        } else if let Some((agent_name, method_name)) =
            split_agent_method(&method_base, &self.agents)
        {
            (agent_name, method_name)
        } else if self.agents.len() == 1 {
            let agent_name = self.agents.keys().next().cloned().unwrap_or_default();
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
struct RunnerRegistry(Arc<AgentRunner>);

#[async_trait]
impl AgentRegistry for RunnerRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.0.discovery_entries()
    }

    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: Value,
    ) -> Result<BusStream<Value>> {
        self.0.handle_a2a_by_key(key, request).await
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

#[derive(Debug, Clone)]
enum ProvenanceStoreKind {
    FalkorDb { url: String, graph: String },
}

#[derive(Debug, Clone)]
struct RunnerConfig {
    packages: Vec<PathBuf>,
    invoke: Option<(String, String, String)>,
    a2a_stdio: bool,
    serve_http: Option<String>,
    provenance_store: ProvenanceStoreKind,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProvenanceStoreChoice {
    Falkordb,
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

    /// Provenance storage backend.
    #[arg(long, value_enum, default_value_t = ProvenanceStoreChoice::Falkordb)]
    provenance_store: ProvenanceStoreChoice,

    /// FalkorDB connection URL (required when provenance store is falkordb).
    #[arg(long)]
    falkordb_url: Option<String>,

    /// FalkorDB graph name (defaults to baml_prov).
    #[arg(long, default_value = "baml_prov")]
    falkordb_graph: String,
}

impl Cli {
    fn into_config(self) -> anyhow::Result<RunnerConfig> {
        let invoke = self
            .invoke
            .map(|values| (values[0].clone(), values[1].clone(), values[2].clone()));

        let provenance_store = match self.provenance_store {
            ProvenanceStoreChoice::Falkordb => {
                let url = self.falkordb_url.ok_or_else(|| {
                    anyhow::anyhow!("--falkordb-url is required for falkordb store")
                })?;
                ProvenanceStoreKind::FalkorDb {
                    url,
                    graph: self.falkordb_graph,
                }
            }
        };

        Ok(RunnerConfig {
            packages: self.packages,
            invoke,
            a2a_stdio: self.a2a_stdio,
            serve_http: self.serve_http,
            provenance_store,
        })
    }
}

fn build_provenance_writer(store: &ProvenanceStoreKind) -> Option<Arc<dyn ProvenanceWriter>> {
    match store {
        ProvenanceStoreKind::FalkorDb { url, graph } => {
            let config = FalkorDbProvenanceConfig::new(url.clone(), graph.clone());
            Some(Arc::new(FalkorDbProvenanceWriter::new(config)))
        }
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
    let provenance_writer = build_provenance_writer(&config.provenance_store);
    let access_allowlist = parse_access_allowlist();
    let tool_index = match &config.provenance_store {
        ProvenanceStoreKind::FalkorDb { url, graph } => {
            Some(ToolIndexConfig::new(url.clone(), graph.clone()))
        }
    };
    let mut runner = AgentRunner::new(provenance_writer, tool_index, access_allowlist);

    for package in &config.packages {
        let package_path = Path::new(package);
        if !package_path.exists() {
            eprintln!("Error: Agent package not found: {}", package_path.display());
            std::process::exit(1);
        }

        match runner.load_agent(package_path).await {
            Ok(_) => {
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

    if let Some((agent_name, function_name, json_args)) = config.invoke {
        let args_value: Value =
            serde_json::from_str(&json_args).context("Invalid JSON arguments")?;
        let result = runner
            .invoke(&agent_name, &function_name, args_value)
            .await
            .context("Function invocation failed")?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // If we get here, just loaded agents without invoking
    let agents = runner.list_agents();
    if agents.is_empty() {
        eprintln!("Error: No agents loaded");
        std::process::exit(1);
    }

    println!("✅ Loaded {} agent(s):", agents.len());
    for agent_name in &agents {
        println!("  - {}", agent_name);
    }

    if let Some(bind) = &config.serve_http {
        let runner = Arc::new(runner);
        let registry: Arc<dyn AgentRegistry> = Arc::new(RunnerRegistry(runner));
        info!(bind = %bind, "A2A server mode: exposing HTTP API (GET /agents, POST /agents/.../a2a, GET /openapi.json)");
        baml_rt_api::serve(registry, bind)
            .await
            .map_err(|e| anyhow::anyhow!("HTTP API server: {e}"))?;
        return Ok(());
    }

    if config.a2a_stdio {
        runner.run_a2a_stdio().await?;
        return Ok(());
    }

    info!("Agent Runner completed successfully");
    Ok(())
}
