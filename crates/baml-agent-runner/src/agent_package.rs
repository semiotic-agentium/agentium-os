//! Agent package boot pipeline: typestate machine from inert tar.gz to live A2aAgent.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentLister, AgentManifest, BamlRtError,
    DeploymentContentHash, Result, bus::BusStream, context::InvocationScope, ids::AgentId,
};
use baml_rt_observability::spans;
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, ToolIndexConfig, index_tools};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, SecretResolverToLlmAdapter};
use baml_rt_tools::{ManifestToolNames, ToolAccessPolicy, register_manifest_tools};
use baml_rt_tools_claude::{AgentWorkspaceRegistry, ClaudeSessionBundle};
use baml_tools_system::SystemBundle;
use serde_json::Value;
use tracing::{error, info};

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
    ) -> Result<ToolsRegistered> {
        let mut runtime_manager = loaded.runtime_manager;
        let manifest_tool_names = ManifestToolNames::parse(&self.manifest.tools)?;

        let tool_registry = runtime_manager.tool_registry();
        tool_registry.register_bundle(SystemBundle::new_with_provenance(
            agent_list_catalogue,
            tool_registry.clone(),
            a2a_handler,
            provenance_query,
        ))?;

        let claude_workspace_root = match std::env::var("BAML_CLAUDE_WORKSPACES_BASE") {
            Ok(ref base) if !base.trim().is_empty() => {
                let path = std::path::PathBuf::from(base.trim());
                let absolute = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "current_dir failed, using .");
                            std::path::PathBuf::from(".")
                        })
                        .join(path)
                };
                std::fs::create_dir_all(&absolute).map_err(BamlRtError::Io)?;
                let canonical = std::fs::canonicalize(&absolute).unwrap_or_else(|e| {
                    tracing::warn!(path = %absolute.display(), error = %e, "canonicalize failed");
                    absolute
                });
                info!(
                    env = base.trim(), base = %canonical.display(),
                    "Claude workspaces root from BAML_CLAUDE_WORKSPACES_BASE (persistent)",
                );
                canonical
            }
            _ => {
                let fallback = self.extract_dir.join(".claude-workspaces");
                info!(
                    base = %fallback.display(),
                    "Claude workspaces root under extract dir (BAML_CLAUDE_WORKSPACES_BASE unset or empty).",
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
                    tracing::warn!(error = %e, "Agent code execution returned an error (may be expected)");
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
            let manager = runtime_manager_arc.read().await;
            let tools = manager.export_tool_metadata().await;
            if let Err(err) = index_tools(provenance_config.store().as_ref(), &tools).await {
                tracing::warn!(error = %err, "Failed to index tool metadata in provenance store");
            } else {
                info!("Tool metadata indexed in provenance store");
            }
        }

        let agent_id = agent.agent_id().clone();
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
            error!(error = ?e, agent_id = %agent_id, "Failed to write AgentBooted event");
        } else {
            info!(agent_id = %agent_id, "AgentBooted event written to provenance store");
        }

        Ok((agent, agent_id))
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

/// Booted agent — holds the running A2aAgent and full manifest for discovery.
#[derive(Clone)]
pub(crate) struct BootedAgent {
    pub(crate) agent: A2aAgent,
    pub(crate) manifest: AgentManifest,
    pub(crate) baml_functions: Vec<String>,
    pub(crate) content_hash: Option<DeploymentContentHash>,
    pub(crate) repository_version: Option<u32>,
}

impl BootedAgent {
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

/// Frozen snapshot of discovery entries used during boot to avoid listing the agent being booted.
#[derive(Clone)]
pub(crate) struct SnapshotAgentLister {
    pub(crate) entries: Vec<AgentDiscoveryEntry>,
}

impl AgentLister for SnapshotAgentLister {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}
