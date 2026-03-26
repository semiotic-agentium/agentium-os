//! BAML Agent Runner
//!
//! This binary loads and executes one or more packaged agent applications.
//! Each agent package is a tar.gz containing BAML schemas, compiled TypeScript,
//! and metadata.

#![recursion_limit = "256"]

mod builder;
mod deployment_state;
mod package;
#[cfg(feature = "slack")]
mod slack_event_producer;

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
    AgentManifest, AgentPackageName, AgentRouteKey, BamlRtError, ContextId, DeployResult,
    DeploymentContentHash, DeploymentManager, DeploymentRecord, DeploymentStatus, Result,
    RuntimeScope, UndeployResult,
    bus::BusStream,
    collect_a2a_stream,
    context::{self, InvocationScope},
    ids::{AgentId, DerivedId, ExternalId, TaskId},
    route_key_from_request,
};
use baml_rt_llm_config::{
    FnoxFileSecretResolver, OverlaySecretResolver, SECRET_LINKS_CONFIG_KEY, SecretLinksState,
    apply_secret_links_state,
};
use baml_rt_observability::{spans, tracing_setup};
use baml_rt_provenance::{
    AgentType, GraphExporter, ProvEvent, ProvenanceOpsFilters, ProvenanceOpsQuery,
    ProvenanceOpsQueryRequest, ProvenanceOpsResource, ProvenancePlanningQuery, ProvenanceWriter,
    SurrealStoreBuilder, ToolIndexConfig, context_metrics_queries,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
    index_tools,
};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, SecretResolverToLlmAdapter};
use baml_rt_repository::{
    BlobStore, LineageStore, MetadataStore, RepositoryService, SearchStore, SurrealStore,
};
use baml_rt_tools::{
    InventoryCatalog, ManifestToolNames, ToolAccessPolicy, parse_access_allowlist,
    register_manifest_tools,
};
use baml_rt_tools_claude::{AgentWorkspaceRegistry, ClaudeSessionBundle};
use baml_tools_calculator as _;
#[cfg(feature = "clickup")]
use baml_tools_clickup as _;
#[cfg(feature = "memory")]
use baml_tools_memory as _;
#[cfg(feature = "notion")]
use baml_tools_notion as _;
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use baml_tools_system::SystemBundle;
use clap::Parser;
use serde_json::Value;
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

    async fn load_schema_phase(
        &self,
        _provenance_config: &ProvenanceConfig,
    ) -> Result<SchemaLoaded> {
        let mut runtime_manager = BamlRuntimeManager::builder().build()?;
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
        provenance_query: Arc<dyn ProvenanceOpsQuery>,
    ) -> Result<ToolsRegistered> {
        let mut runtime_manager = loaded.runtime_manager;
        let manifest_tool_names = ManifestToolNames::parse(&self.manifest.tools)?;

        // Host composes tool catalogue:
        // - system bundle (internal_a2a, discover_agents, discover_tools)
        // - claude bundle (claude/dev host-managed stream session)
        let tool_registry = runtime_manager.tool_registry();
        tool_registry.register_bundle(SystemBundle::new_with_provenance(
            agent_list_catalogue,
            tool_registry.clone(),
            a2a_handler,
            provenance_query,
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
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "current_dir failed, using .");
                            PathBuf::from(".")
                        })
                        .join(path)
                };
                std::fs::create_dir_all(&absolute).map_err(BamlRtError::Io)?;
                let canonical = std::fs::canonicalize(&absolute).unwrap_or_else(|e| {
                    tracing::warn!(path = %absolute.display(), error = %e, "canonicalize failed");
                    absolute
                });
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

        runtime_manager.rebuild_function_tool_manifest();

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

        registered
            .runtime_manager
            .tool_registry()
            .set_config_resolver(Some(provenance_config.config_service()));
        let mut runtime_manager = registered.runtime_manager;
        runtime_manager.set_llm_secret_resolver(Arc::new(SecretResolverToLlmAdapter::new(
            provenance_config.llm_secret_resolver(),
        )));

        // Wire per-agent/per-prompt LLM client overrides from the config store.
        {
            use baml_rt_llm_config::{LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, StaticResolver};
            let config_service = provenance_config.config_service();
            let bundle = baml_rt_tools::BundleName::new(LLM_CONFIG_BUNDLE_NAME)
                .expect("llm bundle name valid");
            let llm_config = match config_service.get(&bundle).await {
                Ok(Some(v)) => match LlmClientConfig::from_value(v) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "stored LLM config parse failed; using sensible default for overrides");
                        LlmClientConfig::sensible_default()
                    }
                },
                Ok(None) => LlmClientConfig::sensible_default(),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load LLM config from store; using sensible default for overrides");
                    LlmClientConfig::sensible_default()
                }
            };
            tracing::info!(
                default = %llm_config.default,
                clients = llm_config.clients.len(),
                agent_overrides = llm_config.overrides.agent.len(),
                function_overrides = llm_config.overrides.agent_function.len(),
                "LLM client config loaded for override resolution"
            );
            let resolver = Arc::new(StaticResolver::new(
                Arc::new(llm_config),
                provenance_config.llm_secret_resolver(),
            ));
            runtime_manager.set_llm_client_resolver(resolver);
        }

        let runtime_manager_arc = Arc::new(Mutex::new(runtime_manager));
        let quickjs_config = QuickJSConfig::new().with_stream_collector_idle_secs(stream_idle_secs);
        let mut agent_builder = A2aAgent::builder()
            .with_runtime_handle(runtime_manager_arc.clone())
            .with_quickjs_config(quickjs_config)
            .with_baml_helpers(true)
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()));

        agent_builder = agent_builder.with_surreal_store(provenance_config.store().clone());

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
            match bridge_guard.eval_sync(&agent_code).await {
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
        _tool_index: Option<ToolIndexConfig>,
        policy: &ToolAccessPolicy,
        agent_list_catalogue: Arc<dyn AgentLister>,
        a2a_handler: Arc<dyn A2aRequestHandler>,
        stream_idle_secs: Option<u64>,
    ) -> Result<(A2aAgent, AgentId)> {
        let span = spans::load_agent_package(&self.extract_dir);
        let _guard = span.enter();
        let loaded = self.load_schema_phase(provenance_config).await?;
        let registered = self
            .register_tools_phase(
                loaded,
                policy,
                agent_list_catalogue,
                a2a_handler,
                provenance_config.store().clone(),
            )
            .await?;
        let built = self
            .build_agent_phase(registered, provenance_config, stream_idle_secs)
            .await?;
        let initialized = self.initialize_js_phase(built).await?;
        let agent = initialized.agent;
        let runtime_manager_arc = initialized.runtime_manager;

        {
            let manager = runtime_manager_arc.lock().await;
            let tools = manager.export_tool_metadata().await;
            if let Err(err) = index_tools(provenance_config.store().as_ref(), &tools).await {
                warn!(error = %err, "Failed to index tool metadata in provenance store");
            } else {
                info!("Tool metadata indexed in provenance store");
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
    /// BAML function names captured from the runtime at boot time (synchronous copy).
    baml_functions: Vec<String>,
    content_hash: Option<DeploymentContentHash>,
    repository_version: Option<u32>,
}

impl BootedAgent {
    /// Manifest version (for discovery card and listing).
    fn version(&self) -> &str {
        &self.manifest.version
    }

    async fn invoke_function(&self, function_name: &str, args: Value) -> Result<Value> {
        let scope = InvocationScope::synthetic_message(self.agent.agent_id().clone());
        QuickJSBridge::invoke_js_function_nonblocking(
            self.agent.bridge(),
            &scope,
            function_name,
            args,
        )
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
/// RwLock poison is treated as fatal (unrecoverable); we do not handle poisoning.
pub(crate) struct AgentRunner {
    agents: RwLock<HashMap<String, BootedAgent>>,
    provenance_config: ProvenanceConfig,
    deployment_state: Arc<deployment_state::DeploymentStateStore>,
    tool_index: Option<ToolIndexConfig>,
    access_policy: ToolAccessPolicy,
    routed_agents: std::sync::RwLock<HashMap<AgentRouteKey, A2aAgent>>,
    internal_a2a_router: Arc<InternalA2aRouter>,
    stream_idle_secs: Option<u64>,
    repository_url: String,
    repository_http_client: reqwest::Client,
}

impl AgentRunner {
    pub(crate) fn new(
        provenance_config: ProvenanceConfig,
        deployment_state: Arc<deployment_state::DeploymentStateStore>,
        tool_index: Option<ToolIndexConfig>,
        access_policy: ToolAccessPolicy,
        stream_idle_secs: Option<u64>,
        repository_url: String,
    ) -> Self {
        let routed_agents = std::sync::RwLock::new(HashMap::new());
        let internal_a2a_router = Arc::new(InternalA2aRouter::new());
        Self {
            agents: RwLock::new(HashMap::new()),
            provenance_config,
            deployment_state,
            tool_index,
            access_policy,
            routed_agents,
            internal_a2a_router,
            stream_idle_secs,
            repository_url,
            repository_http_client: reqwest::Client::new(),
        }
    }

    pub(crate) fn deployment_state(&self) -> &Arc<deployment_state::DeploymentStateStore> {
        &self.deployment_state
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

    async fn fetch_blob_from_repository(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<Vec<u8>> {
        let url = format!(
            "{}/blobs/{}",
            self.repository_url.trim_end_matches('/'),
            content_hash.as_str()
        );
        let response = self
            .repository_http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "repository GET failed at {url}: {e}"
                )))
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(BamlRtError::AgentNotFound(format!(
                "Repository blob not found for hash {}",
                content_hash.as_str()
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(BamlRtError::Io(std::io::Error::other(format!(
                "repository GET failed ({status}) at {url}: {body}"
            ))));
        }
        response.bytes().await.map(|b| b.to_vec()).map_err(|e| {
            BamlRtError::Io(std::io::Error::other(format!(
                "failed reading repository blob body for {}: {e}",
                content_hash.as_str()
            )))
        })
    }

    async fn fetch_repository_version(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<Option<u32>> {
        let url = format!(
            "{}/entries/{}",
            self.repository_url.trim_end_matches('/'),
            content_hash.as_str()
        );
        let response = self
            .repository_http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "repository GET entry failed at {url}: {e}"
                )))
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(BamlRtError::Io(std::io::Error::other(format!(
                "repository GET entry failed ({status}) at {url}: {body}"
            ))));
        }
        let value = response.json::<serde_json::Value>().await.map_err(|e| {
            BamlRtError::Io(std::io::Error::other(format!(
                "invalid repository entry JSON for {}: {e}",
                content_hash.as_str()
            )))
        })?;
        let version = value
            .get("version_ref")
            .and_then(|v| v.get("version"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        Ok(version)
    }

    fn validate_hash_and_content(content_hash: &DeploymentContentHash, bytes: &[u8]) -> Result<()> {
        let computed = sha256_hex(bytes);
        if computed != content_hash.as_str() {
            return Err(BamlRtError::InvalidArgument(format!(
                "Blob content hash mismatch for {} (computed {computed})",
                content_hash.as_str()
            )));
        }
        Ok(())
    }

    async fn boot_from_blob_bytes(
        &self,
        bytes: &[u8],
        content_hash: &DeploymentContentHash,
        repository_version: Option<u32>,
    ) -> Result<(String, AgentRouteKey, BootedAgent)> {
        let mut package_file = tempfile::Builder::new()
            .prefix("baml-deploy-")
            .suffix(".tar.gz")
            .tempfile()
            .map_err(BamlRtError::Io)?;
        use std::io::Write as _;
        package_file.write_all(bytes).map_err(BamlRtError::Io)?;
        package_file.flush().map_err(BamlRtError::Io)?;

        let package = AgentPackage::load_from_file(package_file.path()).await?;
        let name = package.name().to_string();
        let package_name = AgentPackageName::parse(&name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "Agent package name '{name}' is invalid; allowed characters: [A-Za-z0-9_-]"
            ))
        })?;
        let route_key = AgentRouteKey::new(package_name, AgentInstanceId::default());
        let scoped_router: Arc<dyn A2aRequestHandler> = Arc::new(ScopedInternalA2aRouter::new(
            route_key.clone(),
            self.internal_a2a_router().clone(),
        ));
        // Snapshot is intentional: deploy boot only needs currently visible peers.
        // Agents loaded later are visible on subsequent discovery calls.
        let catalogue = Arc::new(SnapshotAgentLister {
            entries: self.discovery_entries(),
        }) as Arc<dyn AgentLister>;
        let (agent, _agent_id) = package
            .boot(
                self.provenance_config(),
                self.tool_index().clone(),
                self.access_policy(),
                catalogue,
                scoped_router,
                self.stream_idle_secs(),
            )
            .await?;
        let manifest = package.manifest().clone();
        let baml_functions: Vec<String> = {
            let bridge_arc = agent.bridge();
            let bridge = bridge_arc.lock().await;
            let all = bridge.list_baml_functions().await;
            let mut seen = std::collections::HashSet::new();
            all.into_iter()
                .map(|name| {
                    baml_rt_core::BamlFunctionId::parse(&name)
                        .prompt_name()
                        .as_str()
                        .to_string()
                })
                .filter(|name| seen.insert(name.clone()))
                .collect()
        };
        let booted = BootedAgent {
            agent,
            manifest,
            baml_functions,
            content_hash: Some(content_hash.clone()),
            repository_version,
        };
        Ok((name, route_key, booted))
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
                    content_hash: booted.content_hash.as_ref().map(|h| h.as_str().to_string()),
                    repository_version: booted.repository_version,
                    agent_package: pkg.clone(),
                    agent_instance_id: AgentInstanceId::DEFAULT.to_string(),
                    tools: m.tools.clone(),
                    baml_functions: booted.baml_functions.clone(),
                    description: m.discovery.as_ref().and_then(|d| d.description.clone()),
                    capabilities: m
                        .discovery
                        .as_ref()
                        .map(|d| d.capabilities.clone())
                        .unwrap_or_default(),
                    tags: m.tags.clone(),
                    subscriptions: m
                        .discovery
                        .as_ref()
                        .map(|d| d.subscriptions.clone())
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

    pub(crate) async fn handle_dispatch_by_key(
        &self,
        key: &AgentRouteKey,
        request: baml_rt_core::AgentDispatchRequest,
    ) -> Result<baml_rt_core::AgentDispatchAck> {
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
        routed_agent.handle_dispatch(request).await
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
                    let serialized = serialize_a2a_response(&response);
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
                let serialized = serialize_a2a_response(&response);
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
                let serialized = serialize_a2a_response(&response);
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

#[derive(Clone)]
struct SnapshotAgentLister {
    entries: Vec<AgentDiscoveryEntry>,
}

impl AgentLister for SnapshotAgentLister {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

// `?Send` matches core trait contract; deploy boot path is currently local-executor bound.
#[async_trait(?Send)]
impl DeploymentManager for AgentRunner {
    async fn deploy_by_hash(&self, content_hash: &DeploymentContentHash) -> Result<DeployResult> {
        {
            let agents = self.agents.read().expect("RwLock poison");
            if agents
                .values()
                .any(|agent| agent.content_hash.as_ref() == Some(content_hash))
            {
                return Ok(DeployResult {
                    already_deployed: true,
                });
            }
        }

        let bytes = self.fetch_blob_from_repository(content_hash).await?;
        AgentRunner::validate_hash_and_content(content_hash, &bytes)?;
        let repository_version = match self.fetch_repository_version(content_hash).await {
            Ok(version) => version,
            Err(err) => {
                warn!(
                    error = %err,
                    content_hash = %content_hash.as_str(),
                    "Failed to resolve repository version for deployment; continuing without repository_version"
                );
                None
            }
        };
        let (name, route_key, booted) = self
            .boot_from_blob_bytes(&bytes, content_hash, repository_version)
            .await?;

        {
            let mut agents = self.agents.write().expect("RwLock poison");
            if agents
                .values()
                .any(|agent| agent.content_hash.as_ref() == Some(content_hash))
            {
                return Ok(DeployResult {
                    already_deployed: true,
                });
            }
            if let Some(existing) = agents.get(&name)
                && existing.content_hash.as_ref() != Some(content_hash)
            {
                return Err(BamlRtError::Conflict(format!(
                    "Agent '{name}' is already loaded with a different content hash"
                )));
            }

            let mut routed = self.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key, booted.agent.clone());
            agents.insert(name.clone(), booted);
        }

        let now = unix_timestamp_secs();
        self.deployment_state
            .save_deployment(&DeploymentRecord {
                content_hash: content_hash.clone(),
                agent_name: name,
                deployed_at: now.clone(),
                status: DeploymentStatus::Active,
                last_error: None,
                last_attempt_at: Some(now),
                failure_count: 0,
            })
            .await?;

        Ok(DeployResult {
            already_deployed: false,
        })
    }

    async fn undeploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<UndeployResult> {
        let removed = {
            let mut agents = self.agents.write().expect("RwLock poison");
            let target_name = agents.iter().find_map(|(name, agent)| {
                (agent.content_hash.as_ref() == Some(content_hash)).then_some(name.clone())
            });
            let Some(target_name) = target_name else {
                return Ok(UndeployResult { removed: false });
            };
            let Some(booted) = agents.remove(&target_name) else {
                return Ok(UndeployResult { removed: false });
            };
            drop(agents);

            if let Some(package_name) = AgentPackageName::parse(&booted.manifest.name) {
                let route_key = AgentRouteKey::new(package_name, AgentInstanceId::default());
                let mut routed = self.routed_agents.write().expect("RwLock poison");
                routed.remove(&route_key);
            }
            true
        };

        if removed {
            self.deployment_state
                .remove_deployment(content_hash)
                .await?;
        }
        Ok(UndeployResult { removed })
    }

    async fn list_deployments(&self) -> Result<Vec<DeploymentRecord>> {
        self.deployment_state.list_deployments().await
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

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: baml_rt_core::AgentDispatchRequest,
    ) -> Result<baml_rt_core::AgentDispatchAck> {
        self.0.handle_dispatch_by_key(key, request).await
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

    /// Wire to the runner after construction. The builder always calls this before any route;
    /// route_from may assume the runner is set.
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
        // Builder guarantees set_runner is called before any route (see set_runner doc).
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

/// Serialize an A2A JSON-RPC response for stdio; on failure returns a minimal error JSON line.
fn serialize_a2a_response(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
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
            blocking: Some(false),
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

fn unix_timestamp_secs() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_secs().to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Provenance store: in-memory (default) or file-backed embedded SurrealDB (SurrealKV directory).
#[derive(Debug, Clone)]
enum ProvenanceDb {
    InMemory,
    File(PathBuf),
}

#[derive(Debug, Clone)]
struct RunnerConfig {
    packages: Vec<PathBuf>,
    repository_url: String,
    repository_dir: PathBuf,
    invoke: Option<(String, String, String)>,
    a2a_stdio: bool,
    serve_http: Option<String>,
    web_dir: Option<PathBuf>,
    provenance_db: ProvenanceDb,
    state_dir: PathBuf,
    /// If set, used as Claude workspaces root (overrides BAML_CLAUDE_WORKSPACES_BASE env).
    claude_workspaces_base: Option<PathBuf>,
    /// Stream collector idle timeout in seconds. No yield for this long ends the stream (Timeout). Default 900 for long-running tool sessions (e.g. claude/dev).
    stream_idle_secs: Option<u64>,
    /// Event producer poll interval. `None` disables the poll loop.
    event_poll_interval: Option<std::time::Duration>,
    /// Slack channel to poll for events (name or ID). Requires event_poll_interval.
    #[cfg(feature = "slack")]
    slack_event_channel: Option<String>,
}

#[derive(Debug, Parser)]
#[command(name = "baml-agent-runner")]
#[command(about = "Load and execute one or more packaged agents", long_about = None)]
struct Cli {
    /// Agent package tar.gz paths to load.
    #[arg(value_name = "AGENT_PACKAGE")]
    packages: Vec<PathBuf>,

    /// Repository base URL used for hash-based deploy/restore (e.g. http://127.0.0.1:8080/repository).
    #[arg(
        long,
        value_name = "URL",
        default_value = "http://127.0.0.1:8080/repository"
    )]
    repository_url: String,

    /// Local repository data directory (embedded SurrealKV backing /repository routes).
    #[arg(long, value_name = "DIR", default_value = "./.repository")]
    repository_dir: PathBuf,

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

    /// Provenance storage path: `:memory:` or a directory for embedded SurrealKV (config lives alongside as `config.db`).
    #[arg(long, value_name = "PATH", default_value = ":memory:")]
    provenance_db: String,

    /// Runner-local deployment state directory (embedded SurrealKV for deployment metadata/state).
    #[arg(long, value_name = "DIR", default_value = "./.runner-state")]
    state_dir: PathBuf,

    /// Claude workspaces root directory (claude/dev session cwd base). When set, overrides BAML_CLAUDE_WORKSPACES_BASE. Use an absolute path or path relative to current working directory.
    #[arg(long, value_name = "DIR")]
    claude_workspaces_base: Option<PathBuf>,

    /// Stream collector idle timeout (seconds). If no chunk is yielded for this long, the stream ends with Timeout. Default 900 for long-running tool sessions (e.g. claude/dev).
    #[arg(long, value_name = "SECS", default_value = "900")]
    stream_idle_secs: u64,

    /// Event producer poll interval (seconds). When non-zero, the runner polls
    /// registered event producers and delivers events to subscribed agents.
    /// 0 disables the poll loop (default).
    #[arg(long, value_name = "SECS", default_value = "0")]
    event_poll_interval_secs: u64,

    /// Slack channel to poll for events (name or ID). Requires --event-poll-interval-secs.
    /// Needs SLACK_BOT_TOKEN or SLACK_USER_TOKEN in the environment.
    #[cfg(feature = "slack")]
    #[arg(long, value_name = "CHANNEL")]
    slack_event_channel: Option<String>,
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
            repository_url: self.repository_url,
            repository_dir: self.repository_dir,
            invoke,
            a2a_stdio: self.a2a_stdio,
            serve_http: self.serve_http,
            web_dir: self.web_dir,
            provenance_db,
            state_dir: self.state_dir,
            claude_workspaces_base: self.claude_workspaces_base,
            stream_idle_secs: Some(self.stream_idle_secs),
            event_poll_interval: if self.event_poll_interval_secs > 0 {
                Some(std::time::Duration::from_secs(
                    self.event_poll_interval_secs,
                ))
            } else {
                None
            },
            #[cfg(feature = "slack")]
            slack_event_channel: self.slack_event_channel,
        })
    }
}

/// Provenance configuration: SurrealDB store with required config and secret services.
pub(crate) enum ProvenanceConfig {
    Surreal {
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
        /// Config store for registry (session open) and HTTP API. Required; use builder to guarantee.
        config_service: Arc<dyn baml_rt_config::ConfigService>,
        /// Secret resolver for LLM config (same mechanism as configuration system; not env vars). Required; use builder to guarantee.
        llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
        /// When Some, PUT /config/secrets/{name} is enabled (UI provisioning). Usually the same Arc as llm_secret_resolver when it is an OverlaySecretResolver.
        runtime_secret_store: Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>>,
    },
}

impl ProvenanceConfig {
    pub(crate) fn store(&self) -> &Arc<baml_rt_provenance::SurrealProvenanceStore> {
        let ProvenanceConfig::Surreal { store, .. } = self;
        store
    }

    pub(crate) fn mermaid_cache(&self) -> Option<Arc<baml_rt_provenance::MermaidCache>> {
        let ProvenanceConfig::Surreal { mermaid_cache, .. } = self;
        mermaid_cache.clone()
    }

    pub(crate) fn config_service(&self) -> Arc<dyn baml_rt_config::ConfigService> {
        let ProvenanceConfig::Surreal { config_service, .. } = self;
        config_service.clone()
    }

    pub(crate) fn llm_secret_resolver(&self) -> Arc<dyn baml_rt_llm_config::SecretResolver> {
        let ProvenanceConfig::Surreal {
            llm_secret_resolver,
            ..
        } = self;
        llm_secret_resolver.clone()
    }

    pub(crate) fn runtime_secret_store(
        &self,
    ) -> Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>> {
        let ProvenanceConfig::Surreal {
            runtime_secret_store,
            ..
        } = self;
        runtime_secret_store.clone()
    }
}

/// Linear builder for provenance config. Call `with_config_service` and `with_llm_secret_resolver` to satisfy required dependencies, then `build`.
pub(crate) struct ProvenanceConfigBuilder {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
    config_service: Option<Arc<dyn baml_rt_config::ConfigService>>,
    llm_secret_resolver: Option<Arc<dyn baml_rt_llm_config::SecretResolver>>,
    runtime_secret_store: Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>>,
}

impl ProvenanceConfigBuilder {
    /// Start building from the given store (and optional mermaid cache). You must then call `with_config_service` and `with_llm_secret_resolver` before `build`.
    fn new(
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        mermaid_cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
    ) -> Self {
        Self {
            store,
            mermaid_cache,
            config_service: None,
            llm_secret_resolver: None,
            runtime_secret_store: None,
        }
    }

    /// Set the config service (required). Consumes `self` and returns the builder for chaining.
    pub(crate) fn with_config_service(
        mut self,
        config_service: Arc<dyn baml_rt_config::ConfigService>,
    ) -> Self {
        self.config_service = Some(config_service);
        self
    }

    /// Set the LLM secret resolver (required; same mechanism as configuration system). Consumes `self` and returns the builder for chaining.
    pub(crate) fn with_llm_secret_resolver(
        mut self,
        llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
    ) -> Self {
        self.llm_secret_resolver = Some(llm_secret_resolver);
        self
    }

    /// Set the runtime secret store (optional). When set, PUT /config/secrets/{name} is enabled for UI provisioning. Use the same Arc as the overlay when using OverlaySecretResolver.
    pub(crate) fn with_runtime_secret_store(
        mut self,
        runtime_secret_store: Option<Arc<dyn baml_rt_llm_config::RuntimeSecretStore>>,
    ) -> Self {
        self.runtime_secret_store = runtime_secret_store;
        self
    }

    /// Build provenance config. Returns `Err` if required dependencies were not set.
    pub(crate) fn build(self) -> Result<ProvenanceConfig> {
        let config_service = self.config_service.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "ProvenanceConfigBuilder: config_service required (call with_config_service)"
                    .into(),
            )
        })?;
        let llm_secret_resolver = self.llm_secret_resolver.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "ProvenanceConfigBuilder: llm_secret_resolver required (call with_llm_secret_resolver)".into(),
            )
        })?;
        Ok(ProvenanceConfig::Surreal {
            store: self.store,
            mermaid_cache: self.mermaid_cache,
            config_service,
            llm_secret_resolver,
            runtime_secret_store: self.runtime_secret_store,
        })
    }
}

/// Build the store and a linear builder for provenance config. Caller must call `with_config_service` and `with_llm_secret_resolver` then `build`.
async fn provenance_config_builder(db: &ProvenanceDb) -> Result<ProvenanceConfigBuilder> {
    match db {
        ProvenanceDb::InMemory => {
            let store = SurrealStoreBuilder::in_memory()
                .build()
                .await
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!(
                        "Provenance in-memory store failed to build: {e}",
                    ))
                })?;
            Ok(ProvenanceConfigBuilder::new(store, None))
        }
        ProvenanceDb::File(path) => {
            let cache = baml_rt_provenance::MermaidCache::new();
            let store = SurrealStoreBuilder::file(path)
                .with_mermaid_cache(cache.clone())
                .build()
                .await
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!(
                        "Provenance file store failed to build at {}: {:#}",
                        path.display(),
                        anyhow::Error::from(e),
                    ))
                })?;
            Ok(ProvenanceConfigBuilder::new(store, Some(cache)))
        }
    }
}

/// Mermaid diagram service backed by SurrealDB provenance. Exported when runner serves HTTP.
/// Uses in-process GraphExporter.
/// Cache avoids repeated export + simplify + render on repeat requests for the same context.
struct MermaidServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
}

impl MermaidServiceImpl {
    fn new(
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
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
        if let Some(ref cache) = self.cache
            && let Some(cached) = cache.get(context_id)
        {
            tracing::debug!(context_id = %context_id, "mermaid: cache HIT");
            return Ok(cached);
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

/// Context metrics service backed by SurrealDB provenance.
struct ContextMetricsServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ContextMetricsServiceImpl {
    fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }
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
        let turn_rows = context_metrics_queries::turn_totals_by_context(&self.store, context_id)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                    e.to_string(),
                )))
            })?;

        let session_rows =
            context_metrics_queries::session_totals_by_context(&self.store, context_id)
                .await
                .map_err(|e| {
                    baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                        e.to_string(),
                    )))
                })?;

        let prompt_rows = context_metrics_queries::user_prompts_by_context(&self.store, context_id)
            .await
            .map_err(|e| {
                baml_rt_api::ContextMetricsError::Other(Box::new(std::io::Error::other(
                    e.to_string(),
                )))
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

struct ProvenanceOpsServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ProvenanceOpsServiceImpl {
    fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl baml_rt_api::ProvenanceOpsService for ProvenanceOpsServiceImpl {
    async fn query(
        &self,
        request: baml_rt_provenance::ProvenanceOpsQueryRequest,
    ) -> std::result::Result<
        baml_rt_provenance::ProvenanceOpsQueryResponse,
        baml_rt_api::ProvenanceOpsError,
    > {
        self.store
            .query_ops(request)
            .await
            .map_err(|e| baml_rt_api::ProvenanceOpsError::Other(Box::new(std::io::Error::other(e))))
    }
}

struct PlanningServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl PlanningServiceImpl {
    fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }

    fn summarize_steps(
        plan: Option<&baml_rt_provenance::PlanningPlanRecord>,
    ) -> baml_rt_api::PlanningStepSummary {
        let mut summary = baml_rt_api::PlanningStepSummary {
            total: 0,
            completed: 0,
            failed: 0,
            in_progress: 0,
            pending: 0,
        };
        let Some(plan) = plan else {
            return summary;
        };
        for step in &plan.steps {
            summary.total += 1;
            match step.status.to_ascii_lowercase().as_str() {
                "completed" => summary.completed += 1,
                "failed" => summary.failed += 1,
                "running" | "in_progress" => summary.in_progress += 1,
                _ => summary.pending += 1,
            }
        }
        summary
    }

    /// `drift.citation` may use camelCase (current serde) or legacy snake_case keys.
    fn parse_citation_details_from_drift(
        drift_obj: &serde_json::Value,
    ) -> Vec<baml_rt_api::CitationDetail> {
        let Some(c) = drift_obj.get("citation") else {
            return Vec::new();
        };
        let Some(arr) = c
            .get("perCitation")
            .or_else(|| c.get("per_citation"))
            .and_then(|v| v.as_array())
        else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|item| {
                let raw = item
                    .get("raw")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let n = item.get("n")?.as_u64()? as u32;
                Some(baml_rt_api::CitationDetail {
                    raw,
                    n,
                    is_history: item
                        .get("isHistory")
                        .or_else(|| item.get("is_history"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                    negated: item
                        .get("negated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    similarity: item
                        .get("similarity")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32,
                    activity_anchor: item
                        .get("activityAnchor")
                        .or_else(|| item.get("activity_anchor"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    content_preview: item
                        .get("contentPreview")
                        .or_else(|| item.get("content_preview"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
            })
            .collect()
    }

    async fn aggregate_drift(
        store: &baml_rt_provenance::SurrealProvenanceStore,
        context_id: &str,
        task_id: &str,
    ) -> Option<baml_rt_api::TaskPlanDriftSummary> {
        use baml_rt_provenance::store::*;

        let report = store
            .query_ops(ProvenanceOpsQueryRequest {
                resource: ProvenanceOpsResource::LlmCalls,
                filters: ProvenanceOpsFilters {
                    context_id: Some(ContextId::from(context_id)),
                    task_id: Some(TaskId::from_external(ExternalId::new(task_id.to_string()))),
                    ..Default::default()
                },
                page_size: Some(100),
                sort_by: Some("timestamp_ms".to_string()),
                sort_dir: Some("desc".to_string()),
                ..Default::default()
            })
            .await
            .ok()?;

        let mut scored_count = 0u32;
        let mut warn_count = 0u32;
        let mut block_count = 0u32;
        let mut latest_plan_drift: Option<&serde_json::Value> = None;
        let mut drifted_calls = Vec::new();

        fn f32_field(obj: &serde_json::Value, key: &str) -> Option<f32> {
            obj.get(key).and_then(|v| v.as_f64()).map(|v| v as f32)
        }
        fn str_field(obj: &serde_json::Value, key: &str) -> String {
            obj.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }

        for row in &report.rows {
            let Some(drift_obj) = row.get("drift") else {
                continue;
            };
            let Some(plan) = drift_obj.get("plan") else {
                continue;
            };
            scored_count += 1;

            if latest_plan_drift.is_none() {
                latest_plan_drift = Some(plan);
            }

            let severity = plan
                .get("compositeSeverity")
                .and_then(|v| v.as_str())
                .unwrap_or("acceptable");
            match severity {
                "warn" => warn_count += 1,
                "block" => block_count += 1,
                _ => {}
            }

            let citations = Self::parse_citation_details_from_drift(drift_obj);

            drifted_calls.push(baml_rt_api::DriftedCallDetail {
                function_name: row
                    .get("baml_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                severity: severity.to_string(),
                intent_alignment: f32_field(plan, "intentAlignment").unwrap_or(0.0),
                step_alignment: f32_field(plan, "stepAlignment"),
                cross_encoder_step_score: f32_field(plan, "crossEncoderStepScore"),
                intent_text_preview: str_field(drift_obj, "intentTextPreview"),
                response_text_preview: str_field(drift_obj, "responseTextPreview"),
                step_text_preview: str_field(drift_obj, "stepTextPreview"),
                citations,
            });
        }

        let plan = latest_plan_drift?;

        Some(baml_rt_api::TaskPlanDriftSummary {
            composite_severity: plan
                .get("compositeSeverity")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            intent_alignment: f32_field(plan, "intentAlignment"),
            step_alignment: f32_field(plan, "stepAlignment"),
            trajectory_drift: f32_field(plan, "trajectoryDrift"),
            plan_adherence_score: f32_field(plan, "planAdherenceScore"),
            scored_call_count: scored_count,
            warn_count,
            block_count,
            drifted_calls,
        })
    }
}

#[async_trait::async_trait]
impl baml_rt_api::PlanningService for PlanningServiceImpl {
    async fn planning_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<baml_rt_api::ContextPlanningResponse, baml_rt_api::PlanningError> {
        let report = self
            .store
            .query_ops(ProvenanceOpsQueryRequest {
                resource: ProvenanceOpsResource::Messages,
                filters: ProvenanceOpsFilters {
                    context_id: Some(ContextId::from(context_id)),
                    ..Default::default()
                },
                page_size: Some(500),
                sort_by: Some("timestamp_ms".to_string()),
                sort_dir: Some("asc".to_string()),
                ..Default::default()
            })
            .await
            .map_err(|e| baml_rt_api::PlanningError::Other(Box::new(std::io::Error::other(e))))?;

        let mut seen = HashSet::new();
        let mut task_ids = Vec::new();
        for row in report.rows {
            let Some(row_obj) = row.as_object() else {
                continue;
            };
            let Some(task_id) = row_obj.get("task_id").and_then(|v| v.as_str()) else {
                continue;
            };
            if seen.insert(task_id.to_string()) {
                task_ids.push(task_id.to_string());
            }
        }

        if task_ids.is_empty() {
            return Err(baml_rt_api::PlanningError::NotFound);
        }

        let mut tasks = Vec::new();
        for task_id_raw in task_ids {
            let task_id = TaskId::from_external(ExternalId::new(task_id_raw.clone()));
            let current_intent = self
                .store
                .query_current_intent(&task_id)
                .await
                .map_err(|e| {
                    baml_rt_api::PlanningError::Other(Box::new(std::io::Error::other(e)))
                })?;
            let current_plan = self.store.query_current_plan(&task_id).await.map_err(|e| {
                baml_rt_api::PlanningError::Other(Box::new(std::io::Error::other(e)))
            })?;
            let intent_history = self
                .store
                .query_intent_history(&task_id, Some(20))
                .await
                .map_err(|e| {
                    baml_rt_api::PlanningError::Other(Box::new(std::io::Error::other(e)))
                })?;
            let plan_history = self
                .store
                .query_plan_history(&task_id, Some(20))
                .await
                .map_err(|e| {
                    baml_rt_api::PlanningError::Other(Box::new(std::io::Error::other(e)))
                })?;

            if current_intent.is_none()
                && current_plan.is_none()
                && intent_history.is_empty()
                && plan_history.is_empty()
            {
                continue;
            }

            let step_summary = Self::summarize_steps(current_plan.as_ref());
            let drift = Self::aggregate_drift(&self.store, context_id, &task_id_raw).await;
            tasks.push(baml_rt_api::TaskPlanningSnapshot {
                task_id: task_id_raw,
                current_intent,
                current_plan,
                intent_history,
                plan_history,
                step_summary,
                drift,
            });
        }

        if tasks.is_empty() {
            return Err(baml_rt_api::PlanningError::NotFound);
        }

        Ok(baml_rt_api::ContextPlanningResponse {
            context_id: context_id.to_string(),
            tasks,
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
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "current_dir failed, using .");
                    PathBuf::from(".")
                })
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
        let canonical = std::fs::canonicalize(&absolute).unwrap_or_else(|e| {
            tracing::warn!(path = %absolute.display(), error = %e, "canonicalize failed");
            absolute
        });
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
            info!(path = %path.display(), "Provenance backend: SurrealKV directory")
        }
    }
    let config_service: Arc<dyn baml_rt_config::ConfigService> = match &config.provenance_db {
        ProvenanceDb::InMemory => Arc::new(
            baml_rt_config::SurrealConfigStore::in_memory()
                .await
                .context("Failed to create in-memory config store")?,
        ),
        ProvenanceDb::File(path) => Arc::new(
            baml_rt_config::SurrealConfigStore::open(
                path.parent()
                    .unwrap_or_else(|| {
                        tracing::debug!(path = %path.display(), "no parent, using path as config base");
                        path.as_ref()
                    })
                    .join("config.db"),
            )
            .await
            .context("Failed to open config store (config.db)")?,
        ),
    };
    let fnox_resolver = Arc::new(FnoxFileSecretResolver::default_path_resolver());
    let overlay = Arc::new(OverlaySecretResolver::new(fnox_resolver.clone()));
    // Apply persisted secret link/unlink state (internal config, not a bundle).
    let link_state: SecretLinksState =
        match config_service.get_internal(SECRET_LINKS_CONFIG_KEY).await {
            Ok(Some(v)) => serde_json::from_value(v).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "secret link state parse failed; using default");
                SecretLinksState::default()
            }),
            Ok(None) => SecretLinksState::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load secret link state; using default");
                SecretLinksState::default()
            }
        };
    apply_secret_links_state(&link_state, overlay.as_ref(), fnox_resolver.as_ref());

    let provenance_config = provenance_config_builder(&config.provenance_db)
        .await
        .context("Failed to initialize provenance storage")?
        .with_config_service(config_service)
        .with_llm_secret_resolver(overlay.clone())
        .with_runtime_secret_store(Some(overlay))
        .build()
        .context("Failed to build provenance config")?;

    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create runner state directory {}",
            config.state_dir.display()
        )
    })?;
    let state_db_path = config.state_dir.join("state.db");
    let deployment_state = Arc::new(
        deployment_state::DeploymentStateStore::open(&state_db_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to initialize runner deployment state DB at {}",
                    state_db_path.display()
                )
            })?,
    );
    std::fs::create_dir_all(&config.repository_dir).with_context(|| {
        format!(
            "Failed to create repository directory {}",
            config.repository_dir.display()
        )
    })?;
    let repository_db_path = config.repository_dir.join("repository.db");
    let repository_store = Arc::new(SurrealStore::open(&repository_db_path).await.with_context(
        || {
            format!(
                "Failed to initialize repository DB at {}",
                repository_db_path.display()
            )
        },
    )?);
    let repository_service = Arc::new(RepositoryService::new(
        repository_store.clone() as Arc<dyn BlobStore>,
        repository_store.clone() as Arc<dyn MetadataStore>,
        repository_store.clone() as Arc<dyn LineageStore>,
        repository_store as Arc<dyn SearchStore>,
    ));
    let existing_deployments = deployment_state
        .list_deployments()
        .await
        .context("Failed to read runner deployment state records")?;
    info!(
        state_dir = %config.state_dir.display(),
        state_db = %state_db_path.display(),
        repository_dir = %config.repository_dir.display(),
        repository_db = %repository_db_path.display(),
        existing_deployments = existing_deployments.len(),
        "Runner deployment + repository backends initialized"
    );

    let access_allowlist = parse_access_allowlist();
    let tool_index = match &config.provenance_db {
        ProvenanceDb::InMemory => Some(ToolIndexConfig::in_memory()),
        ProvenanceDb::File(path) => Some(ToolIndexConfig::new(path)),
    };
    let mut builder = builder::RunnerBuilder::<builder::Loading>::new(
        provenance_config,
        deployment_state,
        tool_index,
        access_allowlist,
        config.stream_idle_secs,
        config.repository_url.clone(),
    );

    for mut deployment in existing_deployments {
        match builder
            .runner
            .deploy_by_hash(&deployment.content_hash)
            .await
        {
            Ok(result) => {
                info!(
                    content_hash = %deployment.content_hash.as_str(),
                    already_deployed = result.already_deployed,
                    "Restored deployment from runner state"
                );
            }
            Err(err) => {
                deployment.status = DeploymentStatus::Failed;
                deployment.last_error = Some(err.to_string());
                deployment.last_attempt_at = Some(unix_timestamp_secs());
                deployment.failure_count = deployment.failure_count.saturating_add(1);
                if let Err(save_err) = builder
                    .runner
                    .deployment_state()
                    .save_deployment(&deployment)
                    .await
                {
                    error!(
                        error = %save_err,
                        content_hash = %deployment.content_hash.as_str(),
                        "Failed to persist restore failure state"
                    );
                }
                warn!(
                    error = %err,
                    content_hash = %deployment.content_hash.as_str(),
                    "Failed to restore deployment; continuing startup"
                );
            }
        }
    }

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
        warn!(
            "No agents loaded at startup (repository restore and package args both empty/failed); runner will continue and can receive deploy requests"
        );
    } else {
        println!("✅ Loaded {} agent(s):", agents.len());
        for agent_name in &agents {
            println!("  - {}", agent_name);
        }
    }

    // --- Event producer poll loop ---
    let dispatcher_handle = if let Some(interval) = config.event_poll_interval {
        let registry = ready.registry();
        let mut dispatcher =
            baml_rt_a2a::EventDispatcher::new(registry as Arc<dyn baml_rt_a2a::AgentRegistry>);

        // Register Slack event producer if configured.
        #[cfg(feature = "slack")]
        if let Some(ref channel) = config.slack_event_channel {
            let producer = slack_event_producer::SlackEventProducer::new(channel.clone())
                .context("creating Slack event producer")?;
            dispatcher
                .register_producer(std::sync::Arc::new(producer))
                .context("registering Slack event producer")?;
            info!(channel = %channel, "registered Slack event producer");
        }

        info!(
            interval_secs = interval.as_secs(),
            "event producer poll loop enabled"
        );
        Some(tokio::spawn(async move {
            run_event_poll_loop(dispatcher, interval).await;
        }))
    } else {
        None
    };

    let http_handle = if let Some(bind) = config.serve_http.clone() {
        let runner = ready.runner();
        let prov_config = runner.provenance_config();
        let store = prov_config.store().clone();
        let config_service = prov_config.config_service();
        let secret_resolver = prov_config.llm_secret_resolver();
        let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> = Arc::new(InventoryCatalog::new());

        let mermaid = Some(Arc::new(MermaidServiceImpl::new(
            store.clone(),
            prov_config.mermaid_cache(),
        )) as Arc<dyn baml_rt_api::MermaidService>);
        let context_metrics = Some(Arc::new(ContextMetricsServiceImpl::new(store.clone()))
            as Arc<dyn baml_rt_api::ContextMetricsService>);
        let provenance_ops = Some(Arc::new(ProvenanceOpsServiceImpl::new(store.clone()))
            as Arc<dyn baml_rt_api::ProvenanceOpsService>);
        let planning = Some(
            Arc::new(PlanningServiceImpl::new(store)) as Arc<dyn baml_rt_api::PlanningService>
        );
        let registry_impl = ready.registry();
        let web_dir = config.web_dir.clone();
        info!(
            bind = %bind,
            web_dir = ?web_dir,
            "A2A server mode: exposing HTTP API (GET /agents, POST /agents/.../a2a/sse, GET /config, GET /contexts/.../mermaid, GET /tasks/.../mermaid, GET /contexts/.../metrics, GET /provenance/..., GET /openapi.json)"
        );
        let runtime_secret_store = prov_config.runtime_secret_store();
        Some(tokio::spawn(async move {
            baml_rt_api::serve_with_services_and_deploy(
                registry_impl,
                &bind,
                mermaid,
                context_metrics,
                provenance_ops,
                planning,
                Some(runner.clone() as Arc<dyn DeploymentManager>),
                Some(config.repository_url.clone()),
                Some(repository_service.clone()),
                tool_catalog,
                config_service,
                secret_resolver,
                runtime_secret_store,
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
        }
        (false, None) => {
            // No stdio, no HTTP. If the dispatcher is running, block on it.
            if let Some(handle) = dispatcher_handle {
                let _ = handle.await;
                return Ok(());
            }
            // else: nothing to run, fall through.
        }
    }

    // Abort event dispatcher on exit from stdio/HTTP paths.
    if let Some(handle) = dispatcher_handle {
        handle.abort();
    }

    info!("Agent Runner completed successfully");
    Ok(())
}

/// Background poll loop for registered event producers.
///
/// Polls all producers, delivers events to matched subscribers, and logs
/// outcomes. Silent when no producers are registered.
async fn run_event_poll_loop(
    mut dispatcher: baml_rt_a2a::EventDispatcher,
    interval: std::time::Duration,
) {
    loop {
        let results = dispatcher.poll_and_deliver().await;
        for (producer_key, outcome) in &results {
            match outcome {
                Ok(delivery) if delivery.failures.is_empty() => {
                    if delivery.subscribers_matched > 0 {
                        info!(
                            producer_key = %producer_key,
                            matched = delivery.subscribers_matched,
                            accepted = delivery.subscribers_accepted,
                            "event delivery complete"
                        );
                    }
                }
                Ok(delivery) => {
                    warn!(
                        producer_key = %producer_key,
                        matched = delivery.subscribers_matched,
                        accepted = delivery.subscribers_accepted,
                        failures = delivery.failures.len(),
                        "event delivery partial failure"
                    );
                }
                Err(err) => {
                    warn!(
                        producer_key = %producer_key,
                        error = %err,
                        "event delivery failed"
                    );
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt_api::PlanningService;
    use baml_rt_core::{
        Citation,
        bus::{BusWithEffects, PlanningSupersessionKind},
        ids::{IntentId, MessageId, PlanId, PlanStepId, UuidId},
        route_key_from_request,
    };
    use baml_rt_llm_config::EmptySecretResolver;
    use serde_json::json;

    use super::*;

    async fn test_provenance_config() -> ProvenanceConfig {
        let config_service = Arc::new(
            baml_rt_config::SurrealConfigStore::in_memory()
                .await
                .expect("in-memory config"),
        );
        provenance_config_builder(&ProvenanceDb::InMemory)
            .await
            .expect("provenance builder")
            .with_config_service(config_service)
            .with_llm_secret_resolver(Arc::new(EmptySecretResolver))
            .build()
            .expect("provenance config")
    }

    async fn test_deployment_state() -> Arc<deployment_state::DeploymentStateStore> {
        Arc::new(
            deployment_state::DeploymentStateStore::open_in_memory()
                .await
                .expect("in-memory deployment state"),
        )
    }

    async fn build_test_agent() -> A2aAgent {
        let manager = BamlRuntimeManager::builder()
            .build()
            .expect("create runtime manager");
        let store = SurrealStoreBuilder::in_memory()
            .build()
            .await
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
            .with_surreal_store(store)
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
            tags: vec![],
            discovery: None,
        };
        runner.insert_agent(
            package_name.to_string(),
            route_key,
            BootedAgent {
                agent: build_test_agent().await,
                manifest,
                baml_functions: vec![],
                content_hash: None,
                repository_version: None,
            },
        );
    }

    #[tokio::test]
    async fn prepare_a2a_request_defaults_to_coordinator_for_plaintext_with_multiple_agents() {
        let runner = AgentRunner::new(
            test_provenance_config().await,
            test_deployment_state().await,
            None,
            ToolAccessPolicy::default(),
            None,
            "http://127.0.0.1:8080/repository".to_string(),
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
    async fn planning_service_for_context_returns_current_and_mixed_history_consistently() {
        let store = SurrealStoreBuilder::in_memory()
            .build()
            .await
            .expect("in-memory store");
        let service = PlanningServiceImpl::new(store.clone());

        let context_id = ContextId::new(120, 1);
        let task_id = TaskId::from_external(ExternalId::new("task-planning-api-mixed-1"));
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000120").unwrap());
        let msg = MessageId::from_external(ExternalId::new("msg-planning-api-mixed-1"));

        store
            .add_event(ProvEvent::agent_booted(
                agent_id.clone(),
                AgentType::new("test").expect("agent type"),
                "1.0.0".to_string(),
                "test@1.0.0".to_string(),
            ))
            .await
            .expect("agent boot");
        store
            .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
            .await
            .expect("task exists");
        store
            .add_event(ProvEvent::task_execution_started(
                context_id.clone(),
                task_id.clone(),
                agent_id.clone(),
            ))
            .await
            .expect("task execution started");
        store
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_id.clone(),
                msg.clone(),
                "user".to_string(),
                vec!["planning service mixed history".to_string()],
                None,
                agent_id,
                1_700_000_020_001,
            ))
            .await
            .expect("message received");

        store
            .add_event(ProvEvent::intent_resolved(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v1".to_string()),
                "seed intent".to_string(),
                vec![Citation::try_new("#1").expect("citation")],
                None,
                None,
            ))
            .await
            .expect("intent v1");
        store
            .add_event(ProvEvent::plan_generated(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v1".to_string()),
                PlanId::from("plan-v1".to_string()),
                vec![baml_rt_provenance::PlanStepSpec {
                    step_id: PlanStepId::from("step-v1".to_string()),
                    description: "step v1".to_string(),
                    order: 0,
                    depends_on: vec![],
                }],
                None,
            ))
            .await
            .expect("plan v1");

        store
            .add_event(ProvEvent::intent_resolved(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v2".to_string()),
                "refined intent".to_string(),
                vec![Citation::try_new("#1").expect("citation")],
                Some(PlanningSupersessionKind::RefinedBy),
                None,
            ))
            .await
            .expect("intent v2");
        store
            .add_event(ProvEvent::plan_generated(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v2".to_string()),
                PlanId::from("plan-v2".to_string()),
                vec![baml_rt_provenance::PlanStepSpec {
                    step_id: PlanStepId::from("step-v2".to_string()),
                    description: "step v2".to_string(),
                    order: 0,
                    depends_on: vec![],
                }],
                Some(PlanningSupersessionKind::RefinedBy),
            ))
            .await
            .expect("plan v2");

        store
            .add_event(ProvEvent::intent_resolved(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v3".to_string()),
                "replacement intent".to_string(),
                vec![Citation::try_new("#1").expect("citation")],
                Some(PlanningSupersessionKind::ReplacedBy),
                None,
            ))
            .await
            .expect("intent v3");
        store
            .add_event(ProvEvent::plan_generated(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v3".to_string()),
                PlanId::from("plan-v3".to_string()),
                vec![baml_rt_provenance::PlanStepSpec {
                    step_id: PlanStepId::from("step-v3".to_string()),
                    description: "step v3".to_string(),
                    order: 0,
                    depends_on: vec![],
                }],
                Some(PlanningSupersessionKind::ReplacedBy),
            ))
            .await
            .expect("plan v3");
        store
            .add_event(ProvEvent::plan_step_status_changed(
                context_id.clone(),
                task_id.clone(),
                IntentId::from("intent-v3".to_string()),
                PlanId::from("plan-v3".to_string()),
                PlanStepId::from("step-v3".to_string()),
                Some("ready".to_string()),
                "in_progress".to_string(),
                vec![Citation::try_new("#1").expect("citation")],
            ))
            .await
            .expect("step status");

        let response = service
            .planning_for_context(context_id.as_str())
            .await
            .expect("planning response");
        assert_eq!(response.context_id, context_id.as_str());
        assert_eq!(response.tasks.len(), 1);

        let task = &response.tasks[0];
        assert_eq!(task.task_id, task_id.as_str());
        assert_eq!(
            task.current_intent
                .as_ref()
                .map(|intent| intent.intent_id.as_str()),
            Some("intent-v3")
        );
        assert_eq!(
            task.current_plan.as_ref().map(|plan| plan.plan_id.as_str()),
            Some("plan-v3")
        );
        assert_eq!(task.intent_history.len(), 3);
        assert_eq!(
            task.intent_history[0].supersession_from_previous,
            Some(PlanningSupersessionKind::ReplacedBy)
        );
        assert_eq!(
            task.intent_history[1].supersession_from_previous,
            Some(PlanningSupersessionKind::RefinedBy)
        );
        assert_eq!(
            task.intent_history[2].superseded_by_next,
            Some(PlanningSupersessionKind::RefinedBy)
        );
        assert_eq!(task.plan_history.len(), 3);
        assert_eq!(
            task.plan_history[0].supersession_from_previous,
            Some(PlanningSupersessionKind::ReplacedBy)
        );
        assert_eq!(
            task.plan_history[1].superseded_by_next,
            Some(PlanningSupersessionKind::ReplacedBy)
        );
        assert_eq!(task.step_summary.total, 1);
        assert_eq!(task.step_summary.in_progress, 1);
        assert_eq!(task.step_summary.completed, 0);
        assert_eq!(task.step_summary.pending, 0);
    }

    #[tokio::test]
    async fn prepare_a2a_request_still_errors_without_coordinator_when_multiple_agents_loaded() {
        let runner = AgentRunner::new(
            test_provenance_config().await,
            test_deployment_state().await,
            None,
            ToolAccessPolicy::default(),
            None,
            "http://127.0.0.1:8080/repository".to_string(),
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
            test_provenance_config().await,
            test_deployment_state().await,
            None,
            ToolAccessPolicy::default(),
            None,
            "http://127.0.0.1:8080/repository".to_string(),
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
            test_provenance_config().await,
            test_deployment_state().await,
            None,
            ToolAccessPolicy::default(),
            None,
            "http://127.0.0.1:8080/repository".to_string(),
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
            tags: vec![],
            discovery: None,
        };
        runner.insert_agent(
            package_name.as_str().to_string(),
            default_key,
            BootedAgent {
                agent,
                manifest,
                baml_functions: vec![],
                content_hash: None,
                repository_version: None,
            },
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
