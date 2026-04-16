//! Agent package boot pipeline: typestate machine from inert tar.gz to live A2aAgent.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentInstanceId, AgentLister,
    AgentManifest, AgentPackageName, BamlRtError, DeploymentContentHash, Result, bus::BusStream,
    context::InvocationScope, ids::AgentId,
};
use baml_rt_observability::spans;
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, index_tools};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, SecretResolverToLlmAdapter};
use baml_rt_tools::{
    BundleRegistrar, ManifestToolNames, ToolAccessPolicy, ToolRegistry, register_manifest_tools,
};
use baml_rt_tools_claude::{AgentWorkspaceRegistry, ClaudeSessionBundle};
use baml_tools_system::SystemBundle;
use serde_json::Value;
use tracing::{Instrument, error, info};

use crate::config::ProvenanceConfig;

/// Inert agent package — holds extracted package data, not yet booted.
pub(crate) struct AgentPackage {
    pub(crate) manifest: AgentManifest,
    pub(crate) extract_dir: PathBuf,
    pub(crate) baml_src: PathBuf,
}

struct SchemaLoaded {
    runtime_manager: BamlRuntimeManager,
}

struct ToolsRegistered {
    runtime_manager: BamlRuntimeManager,
}

pub(crate) struct JsInitialized {
    pub(crate) runtime_manager: Arc<tokio::sync::RwLock<BamlRuntimeManager>>,
    pub(crate) agent: A2aAgent,
}

impl AgentPackage {
    pub(crate) async fn load_from_file(package_path: &Path) -> Result<Self> {
        let (extract_dir, manifest) = crate::package::load_package(package_path).await?;
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
        provenance_query: Arc<dyn baml_rt_provenance::ProvenanceOpsQuery>,
        claude_workspaces_base: Option<&std::path::Path>,
    ) -> Result<ToolsRegistered> {
        let mut runtime_manager = loaded.runtime_manager;
        let manifest_tool_names = ManifestToolNames::parse(&self.manifest.tools)?;

        let tool_registry = runtime_manager.tool_registry();

        // --- Build registrars with pre-injected dependencies ---
        let claude_workspace_root = match claude_workspaces_base {
            Some(base) => {
                info!(
                    base = %base.display(),
                    "Claude workspaces root from --claude-workspaces-base (persistent)",
                );
                base.to_path_buf()
            }
            None => {
                let fallback = self.extract_dir.join(".claude-workspaces");
                info!(
                    base = %fallback.display(),
                    "Claude workspaces root under extract dir (no --claude-workspaces-base).",
                );
                fallback
            }
        };

        #[allow(unused_mut)] // mut needed only when `memory` feature adds to vec
        let mut registrars: Vec<Box<dyn BundleRegistrar>> = vec![
            Box::new(SystemBundleRegistrar {
                agent_list_catalogue,
                tool_registry: tool_registry.clone(),
                a2a_handler,
                provenance_query,
            }),
            Box::new(ClaudeBundleRegistrar {
                workspace_root: claude_workspace_root,
            }),
        ];

        #[cfg(feature = "memory")]
        registrars.push(Box::new(MemoryBundleRegistrar {
            agent_name: self.manifest.name.clone(),
        }));

        // --- Registrar loop: check + register ---
        for registrar in &registrars {
            if registrar.should_register(&self.manifest.tools) {
                registrar.register(&tool_registry)?;
            }
        }

        register_manifest_tools(
            runtime_manager.tool_registry().as_ref(),
            &manifest_tool_names,
            policy,
        )?;
        runtime_manager.rebuild_function_tool_manifest();
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

        {
            use baml_rt_llm_config::{LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, StaticResolver};
            let config_service = provenance_config.config_service();
            let bundle = baml_rt_tools::BundleName::new(LLM_CONFIG_BUNDLE_NAME)
                .expect("llm bundle name valid");
            let llm_config = match config_service.get(&bundle).await {
                Ok(Some(v)) => match LlmClientConfig::from_value(v) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "stored LLM config parse failed; using sensible default");
                        LlmClientConfig::sensible_default()
                    }
                },
                Ok(None) => LlmClientConfig::sensible_default(),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load LLM config; using sensible default");
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

        let runtime_manager_arc = Arc::new(tokio::sync::RwLock::new(runtime_manager));
        let quickjs_config = QuickJSConfig::new().with_stream_collector_idle_secs(stream_idle_secs);
        let agent_package = AgentPackageName::parse(&self.manifest.name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "manifest agent name '{name}' is not a valid agent_package identifier",
                name = self.manifest.name
            ))
        })?;
        let agent_instance_id = AgentInstanceId::default_id();
        let mut agent_builder = A2aAgent::builder()
            .with_runtime_handle(runtime_manager_arc.clone())
            .with_quickjs_config(quickjs_config)
            .with_baml_helpers(true)
            .with_agent_identity(agent_package, agent_instance_id)
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
            let agent_code = std::fs::read_to_string(&entry_point_path).map_err(BamlRtError::Io)?;
            info!(
                entry_point = self.manifest.entry_point,
                "Loading agent JavaScript code"
            );
            async {
                let bridge = built.agent.bridge();
                let mut bridge_guard = bridge.lock().await;
                match bridge_guard.eval_sync(&agent_code).await {
                    Ok(_) => info!("Agent code executed successfully"),
                    Err(e) => {
                        tracing::warn!(error = %e, "Agent code execution returned an error (may be expected)");
                    }
                }
            }
            .instrument(spans::evaluate_agent_code(&self.manifest.entry_point))
            .await;
            info!("Agent JavaScript code loaded and initialized");
        } else {
            info!(
                entry_point = self.manifest.entry_point,
                "Agent entry point not found, skipping JavaScript initialization"
            );
        }
        Ok(built)
    }

    pub(crate) async fn boot(
        &self,
        provenance_config: &ProvenanceConfig,
        policy: &ToolAccessPolicy,
        agent_list_catalogue: Arc<dyn AgentLister>,
        a2a_handler: Arc<dyn A2aRequestHandler>,
        stream_idle_secs: Option<u64>,
        claude_workspaces_base: Option<&std::path::Path>,
    ) -> Result<(A2aAgent, AgentId)> {
        async {
            let loaded = self.load_schema_phase(provenance_config).await?;
            let registered = self
                .register_tools_phase(
                    loaded,
                    policy,
                    agent_list_catalogue,
                    a2a_handler,
                    provenance_config.store().clone(),
                    claude_workspaces_base,
                )
                .await?;
            let built = self
                .build_agent_phase(registered, provenance_config, stream_idle_secs)
                .await?;
            let initialized = self.initialize_js_phase(built).await?;
            let agent = initialized.agent;
            let runtime_manager_arc = initialized.runtime_manager;

            {
                let manager = runtime_manager_arc.read().await;
                let tools = manager.export_tool_metadata().await;
                if let Err(err) = index_tools(provenance_config.store().as_ref(), &tools).await {
                    tracing::warn!(
                        error = %err,
                        "Failed to index tool metadata in provenance store"
                    );
                } else {
                    info!("Tool metadata indexed in provenance store");
                }
            }

            let agent_id = agent.agent_id().clone();
            let writer = provenance_config.store().clone() as Arc<dyn ProvenanceWriter>;
            let archive_path = self.manifest.signature.clone();
            let agent_type_parsed =
                AgentType::new(self.manifest.name.clone()).ok_or_else(|| {
                    BamlRtError::InvalidArgument("agent_type cannot be empty".to_string())
                })?;
            let boot_event = ProvEvent::agent_booted(
                agent_id.clone(),
                agent_type_parsed,
                self.version().to_string(),
                archive_path,
            );
            if let Err(e) = writer.add_event(boot_event).await {
                error!(error = ?e, agent_id = %agent_id, "Failed to write AgentBooted event");
            } else {
                info!(agent_id = %agent_id, "AgentBooted event written to provenance store");
            }

            Ok((agent, agent_id))
        }
        .instrument(spans::load_agent_package(&self.extract_dir))
        .await
    }

    pub(crate) fn name(&self) -> &str {
        &self.manifest.name
    }

    pub(crate) fn version(&self) -> &str {
        &self.manifest.version
    }

    pub(crate) fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }
}

/// Agent lifecycle state — used for drain-before-undeploy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum AgentLifecycleState {
    Active = 0,
    Draining = 1,
}

impl AgentLifecycleState {
    pub(crate) fn from_u8(v: u8) -> Self {
        if v == 1 { Self::Draining } else { Self::Active }
    }
}

/// Tracks how a booted agent was sourced: from the content-addressable
/// repository (production) or constructed inline (tests).
#[derive(Clone, Debug)]
pub(crate) enum DeploymentProvenance {
    Repository {
        content_hash: DeploymentContentHash,
        version: u32,
    },
    #[allow(dead_code)] // constructed in test code only (bin target doesn't see cfg(test) usage)
    Ephemeral,
}

/// Booted agent — holds the running A2aAgent and full manifest for discovery.
#[derive(Clone)]
pub(crate) struct BootedAgent {
    pub(crate) agent: A2aAgent,
    pub(crate) manifest: AgentManifest,
    pub(crate) baml_functions: Vec<String>,
    pub(crate) provenance: DeploymentProvenance,
    pub(crate) lifecycle: Arc<AtomicU8>,
}

impl BootedAgent {
    pub(crate) fn content_hash(&self) -> Option<&DeploymentContentHash> {
        match &self.provenance {
            DeploymentProvenance::Repository { content_hash, .. } => Some(content_hash),
            DeploymentProvenance::Ephemeral => None,
        }
    }

    pub(crate) fn repository_version(&self) -> Option<u32> {
        match &self.provenance {
            DeploymentProvenance::Repository { version, .. } => Some(*version),
            DeploymentProvenance::Ephemeral => None,
        }
    }

    pub(crate) fn lifecycle_state(&self) -> AgentLifecycleState {
        AgentLifecycleState::from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    pub(crate) fn set_draining(&self) {
        self.lifecycle
            .store(AgentLifecycleState::Draining as u8, Ordering::Release);
    }

    pub(crate) fn version(&self) -> &str {
        &self.manifest.version
    }

    pub(crate) async fn invoke_function(&self, function_name: &str, args: Value) -> Result<Value> {
        let scope = InvocationScope::synthetic_message(self.agent.agent_id().clone());
        QuickJSBridge::invoke_js_function_nonblocking(
            self.agent.bridge(),
            &scope,
            function_name,
            args,
        )
        .await
    }

    pub(crate) async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.agent.handle_a2a_stream(request).await
    }
}

/// Fallback discovery list when the internal A2A router has no [`Arc<crate::runner::AgentRunner>`]
/// yet (tests). Prefer [`crate::routing::LiveAgentLister`] in production so `system/discover_agents`
/// matches the live registry after deploy.
#[derive(Clone)]
pub(crate) struct SnapshotAgentLister {
    pub(crate) entries: Vec<AgentDiscoveryEntry>,
}

impl AgentLister for SnapshotAgentLister {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

// ---------------------------------------------------------------------------
// BundleRegistrar implementations — one per built-in bundle.
// ---------------------------------------------------------------------------

/// Registrar for system tools (discover_agents, discover_tools, internal_a2a, etc.).
struct SystemBundleRegistrar {
    agent_list_catalogue: Arc<dyn AgentLister>,
    tool_registry: Arc<ToolRegistry>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
    provenance_query: Arc<dyn baml_rt_provenance::ProvenanceOpsQuery>,
}

impl BundleRegistrar for SystemBundleRegistrar {
    fn name(&self) -> &str {
        "system"
    }

    fn should_register(&self, _manifest_tools: &[String]) -> bool {
        true // system tools always register
    }

    fn register(&self, registry: &ToolRegistry) -> Result<()> {
        registry.register_bundle(SystemBundle::new_with_provenance(
            self.agent_list_catalogue.clone(),
            self.tool_registry.clone(),
            self.a2a_handler.clone(),
            self.provenance_query.clone(),
        ))
    }
}

/// Registrar for Claude session tools (code workspace management).
struct ClaudeBundleRegistrar {
    workspace_root: PathBuf,
}

impl BundleRegistrar for ClaudeBundleRegistrar {
    fn name(&self) -> &str {
        "claude"
    }

    fn should_register(&self, _manifest_tools: &[String]) -> bool {
        true // claude session tools always register
    }

    fn register(&self, registry: &ToolRegistry) -> Result<()> {
        registry.register_bundle(ClaudeSessionBundle::new(Arc::new(
            AgentWorkspaceRegistry::new(self.workspace_root.clone()),
        )))
    }
}

/// Registrar for memory tools (feature-gated, conditional on manifest).
#[cfg(feature = "memory")]
struct MemoryBundleRegistrar {
    agent_name: String,
}

#[cfg(feature = "memory")]
impl BundleRegistrar for MemoryBundleRegistrar {
    fn name(&self) -> &str {
        "memory"
    }

    fn should_register(&self, manifest_tools: &[String]) -> bool {
        manifest_tools.iter().any(|t| t.starts_with("memory/"))
    }

    fn register(&self, registry: &ToolRegistry) -> Result<()> {
        let memory_bundle = baml_tools_memory::MemoryBundle::new(&self.agent_name)?;
        registry.register_bundle(memory_bundle)
    }
}
