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
    AgentManifest, AgentPackageName, BamlRtError, DeploymentContentHash, Result,
    bus::BusStream,
    context::{InvocationScope, generate_context_id},
    ids::AgentId,
};
use baml_rt_observability::spans;
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, index_tools};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, SecretResolverToLlmAdapter};
use baml_rt_tools::{
    BundleRegistrar, ExternalToolResolver, ManifestToolNames, SharedContextRefStore,
    ToolAccessPolicy, ToolRegistry,
    external_tools::{
        BUILDER_EXTERNAL_TOOLS_ENV, DevModeResolver, EXTERNAL_TOOLS_LOCKFILE_NAME,
        ExternalLifecycleEvent, ExternalLifecycleRecorder, ExternalLockfileMode,
        ExternalToolsLockfile,
        resolver::SandboxRuntimeWiring,
        sandbox::{SandboxProvider, fresh_runner_id, stock_wiring_with_bind_roots},
    },
    register_manifest_tools_with_fallback,
};
#[cfg(test)]
use baml_rt_tools::external_tools::sandbox::MockSandboxProvider;
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

/// Inputs for [`AgentPackage::boot`], grouped to satisfy clippy `too_many_arguments`.
pub(crate) struct AgentPackageBootArgs<'a> {
    pub(crate) shared_context_ref_store: SharedContextRefStore,
    pub(crate) provenance_config: &'a ProvenanceConfig,
    pub(crate) policy: &'a ToolAccessPolicy,
    pub(crate) agent_list_catalogue: Arc<dyn AgentLister>,
    pub(crate) a2a_handler: Arc<dyn A2aRequestHandler>,
    pub(crate) stream_idle_secs: Option<u64>,
    pub(crate) claude_workspaces_base: Option<&'a Path>,
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
        shared_context_ref_store: SharedContextRefStore,
        _provenance_config: &ProvenanceConfig,
    ) -> Result<SchemaLoaded> {
        let mut runtime_manager = BamlRuntimeManager::builder()
            .with_shared_context_ref_store(shared_context_ref_store)
            .build()?;
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
        provenance_store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
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
                provenance_query: provenance_store.clone(),
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

        // Dev-mode external tools: BAML_EXTERNAL_TOOLS_DIR=/path/to/tool_a:/path/to/tool_b
        // Each colon-separated entry is a tool package dir containing
        // tool-metadata.json + tool-server binary. Production (Phase 2) uses
        // the digest-pinned lockfile resolver instead.
        let lifecycle_writer: Arc<dyn ProvenanceWriter> = provenance_store.clone();
        let lifecycle_recorder = build_external_lifecycle_recorder(lifecycle_writer);
        let lockfile_path = self.extract_dir.join(EXTERNAL_TOOLS_LOCKFILE_NAME);
        let sandbox_wiring = build_sandbox_wiring()?;
        let external_resolver = build_dev_mode_resolver(
            Some(lifecycle_recorder),
            &lockfile_path,
            ExternalLockfileMode::from_env(),
            sandbox_wiring,
        )
        .await?;

        register_manifest_tools_with_fallback(
            runtime_manager.tool_registry().as_ref(),
            &manifest_tool_names,
            policy,
            external_resolver.as_deref(),
        )?;
        runtime_manager.rebuild_function_tool_manifest();
        runtime_manager
            .set_tool_allowlist(self.manifest.tools.iter().cloned().collect::<HashSet<_>>())
            .await?;

        info!(
            agent = %self.manifest.name,
            manifest_tool_count = manifest_tool_names.len(),
            "manifest tools registered; building A2a runtime next"
        );

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
        info!(
            agent = %self.manifest.name,
            "building QuickJS runtime and A2a bridge (often the longest boot step)"
        );
        let agent = agent_builder.build().await?;
        info!(agent = %self.manifest.name, "QuickJS runtime and A2a bridge ready");
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

    pub(crate) async fn boot(&self, args: AgentPackageBootArgs<'_>) -> Result<(A2aAgent, AgentId)> {
        async {
            info!(
                agent = %self.manifest.name,
                extract_dir = %self.extract_dir.display(),
                "agent package boot starting"
            );
            let loaded = self
                .load_schema_phase(args.shared_context_ref_store, args.provenance_config)
                .await?;
            let registered = self
                .register_tools_phase(
                    loaded,
                    args.policy,
                    args.agent_list_catalogue,
                    args.a2a_handler,
                    args.provenance_config.store().clone(),
                    args.claude_workspaces_base,
                )
                .await?;
            let built = self
                .build_agent_phase(registered, args.provenance_config, args.stream_idle_secs)
                .await?;
            let initialized = self.initialize_js_phase(built).await?;
            let agent = initialized.agent;
            let runtime_manager_arc = initialized.runtime_manager;

            {
                let manager = runtime_manager_arc.read().await;
                let tools = manager.export_tool_metadata().await;
                if let Err(err) = index_tools(args.provenance_config.store().as_ref(), &tools).await
                {
                    tracing::warn!(
                        error = %err,
                        "Failed to index tool metadata in provenance store"
                    );
                } else {
                    info!("Tool metadata indexed in provenance store");
                }
            }

            let agent_id = agent.agent_id().clone();
            let writer = args.provenance_config.store().clone() as Arc<dyn ProvenanceWriter>;
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
// External tool resolver (dev mode).
// ---------------------------------------------------------------------------

fn build_external_lifecycle_recorder(
    writer: Arc<dyn ProvenanceWriter>,
) -> ExternalLifecycleRecorder {
    Arc::new(move |event| {
        let writer = writer.clone();
        tokio::spawn(async move {
            let (tool_name, phase, result, details) = match event {
                ExternalLifecycleEvent::Describe {
                    tool_name,
                    identity,
                    protocol_version,
                    latency_ms,
                    result,
                    details,
                } => (
                    tool_name,
                    "describe".to_string(),
                    result,
                    serde_json::json!({
                        "identity": identity,
                        "protocol_version": protocol_version,
                        "latency_ms": latency_ms,
                        "details": details,
                    }),
                ),
                ExternalLifecycleEvent::Artifact {
                    tool_name,
                    artifact_ref,
                    digest,
                    signer,
                    verification_result,
                    pull_latency_ms,
                    details,
                } => (
                    tool_name,
                    "artifact".to_string(),
                    verification_result,
                    serde_json::json!({
                        "artifact_ref": artifact_ref,
                        "digest": digest,
                        "signer": signer,
                        "pull_latency_ms": pull_latency_ms,
                        "details": details,
                    }),
                ),
                ExternalLifecycleEvent::Quarantine {
                    tool_name,
                    reason,
                    consecutive_failures,
                    started_at_ms,
                } => (
                    tool_name,
                    "quarantine".to_string(),
                    "started".to_string(),
                    serde_json::json!({
                        "reason": reason,
                        "consecutive_failures": consecutive_failures,
                        "started_at_ms": started_at_ms,
                    }),
                ),
                ExternalLifecycleEvent::QuarantineLifted {
                    tool_name,
                    lifted_by,
                    lifted_at_ms,
                } => (
                    tool_name,
                    "quarantine".to_string(),
                    "lifted".to_string(),
                    serde_json::json!({
                        "lifted_by": lifted_by,
                        "lifted_at_ms": lifted_at_ms,
                    }),
                ),
            };

            let event = ProvEvent::external_tool_lifecycle(
                generate_context_id(),
                tool_name,
                phase,
                result,
                details,
            );
            if let Err(e) = writer.add_event(event).await {
                tracing::warn!(error = ?e, "failed to record external tool lifecycle provenance event");
            }
        });
    })
}

/// Build a [`DevModeResolver`] from the `BAML_EXTERNAL_TOOLS_DIR` env var, if set.
///
/// Value is a colon-separated list of tool package directories. Returns `None`
/// when the env var is absent or empty, meaning "no external tools in dev mode".
pub const SANDBOX_BIND_ROOTS_ENV: &str = "BAML_SANDBOX_BIND_ROOTS";

fn build_sandbox_wiring() -> Result<Option<SandboxRuntimeWiring>> {
    let bind_roots: Vec<std::path::PathBuf> = std::env::var(SANDBOX_BIND_ROOTS_ENV)
        .ok()
        .map(|v| {
            v.split(':')
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    #[cfg(feature = "sandbox-provider")]
    {
        use baml_rt_tools::external_tools::sandbox::MicrosandboxProvider;
        let provider: Arc<dyn SandboxProvider> = Arc::new(MicrosandboxProvider::new()?);
        Ok(Some(stock_wiring_with_bind_roots(
            provider,
            fresh_runner_id(),
            bind_roots,
        )))
    }

    #[cfg(all(not(feature = "sandbox-provider"), test))]
    {
        let provider: Arc<dyn SandboxProvider> = Arc::new(MockSandboxProvider::echo());
        Ok(Some(stock_wiring_with_bind_roots(
            provider,
            fresh_runner_id(),
            bind_roots,
        )))
    }

    #[cfg(all(not(feature = "sandbox-provider"), not(test)))]
    {
        let _ = bind_roots;
        Ok(None)
    }
}

async fn build_dev_mode_resolver(
    lifecycle_recorder: Option<ExternalLifecycleRecorder>,
    lockfile_path: &Path,
    lockfile_mode: ExternalLockfileMode,
    sandbox: Option<SandboxRuntimeWiring>,
) -> Result<Option<Box<dyn ExternalToolResolver>>> {
    let lockfile = if lockfile_path.exists() {
        match ExternalToolsLockfile::read_from_path(lockfile_path) {
            Ok(lockfile) => Some(lockfile),
            Err(err) => {
                emit_external_lockfile_event(
                    lifecycle_recorder.as_ref(),
                    "lockfile_parse_error",
                    serde_json::json!({
                        "path": lockfile_path.display().to_string(),
                        "error": err.to_string(),
                        "mode": format!("{lockfile_mode:?}"),
                    }),
                );
                if lockfile_mode.should_enforce() {
                    return Err(err);
                }
                tracing::warn!(
                    error = %err,
                    mode = ?lockfile_mode,
                    path = %lockfile_path.display(),
                    "failed to read external tools lockfile; continuing due to non-enforce mode"
                );
                None
            }
        }
    } else {
        None
    };

    let raw = match std::env::var(BUILDER_EXTERNAL_TOOLS_ENV) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            if let Some(lockfile) = lockfile.as_ref()
                && !lockfile.tools.is_empty()
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "package contains {} external tool lockfile entries at {}, but BAML_EXTERNAL_TOOLS_DIR is not set",
                    lockfile.tools.len(),
                    lockfile_path.display()
                )));
            }
            return Ok(None);
        }
    };

    let dirs: Vec<PathBuf> = raw
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    if dirs.is_empty() {
        if let Some(lockfile) = lockfile.as_ref()
            && !lockfile.tools.is_empty()
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "BAML_EXTERNAL_TOOLS_DIR resolved to zero tool directories while package lockfile {} declares external tools",
                lockfile_path.display()
            )));
        }
        return Ok(None);
    }

    if lockfile.is_none() && lockfile_mode.should_enforce() {
        emit_external_lockfile_event(
            lifecycle_recorder.as_ref(),
            "lockfile_missing",
            serde_json::json!({
                "path": lockfile_path.display().to_string(),
                "mode": format!("{lockfile_mode:?}"),
                "external_dir_count": dirs.len(),
            }),
        );
        return Err(BamlRtError::InvalidArgument(format!(
            "external tools lockfile missing at {} while lockfile mode is enforce",
            lockfile_path.display()
        )));
    }

    info!(
        count = dirs.len(),
        mode = ?lockfile_mode,
        "Loading external tools from BAML_EXTERNAL_TOOLS_DIR"
    );

    let resolver = match sandbox {
        Some(wiring) => {
            DevModeResolver::from_dirs_with_sandbox(
                &dirs,
                lockfile,
                lockfile_mode,
                lifecycle_recorder,
                wiring,
            )
            .await?
        }
        None => {
            DevModeResolver::from_dirs_with_policy(
                &dirs,
                lockfile,
                lockfile_mode,
                lifecycle_recorder,
            )
            .await?
        }
    };
    Ok(Some(Box::new(resolver)))
}

fn emit_external_lockfile_event(
    lifecycle_recorder: Option<&ExternalLifecycleRecorder>,
    verification_result: &str,
    details: serde_json::Value,
) {
    if let Some(recorder) = lifecycle_recorder {
        recorder(ExternalLifecycleEvent::Artifact {
            tool_name: "unknown".to_string(),
            artifact_ref: EXTERNAL_TOOLS_LOCKFILE_NAME.to_string(),
            digest: None,
            signer: None,
            verification_result: verification_result.to_string(),
            pull_latency_ms: None,
            details,
        });
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

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::OnceLock};

    use baml_rt_tools::external_tools::{ExternalToolLockEntry, ExternalToolsLockfile};
    use tempfile::tempdir;

    use super::{
        BUILDER_EXTERNAL_TOOLS_ENV, EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalLockfileMode,
        build_dev_mode_resolver,
    };

    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        static ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn set_env(key: &str, value: &str) {
        // SAFETY: tests in this module serialize env-var mutation via `env_lock`.
        unsafe { std::env::set_var(key, value) }
    }

    fn remove_env(key: &str) {
        // SAFETY: tests in this module serialize env-var mutation via `env_lock`.
        unsafe { std::env::remove_var(key) }
    }

    fn write_tool_fixture(dir: &Path, tool_name: &str) {
        fs::create_dir_all(dir).expect("create tool fixture dir");
        let metadata = serde_json::json!({
            "tool_abi_version": "1",
            "name": tool_name,
            "description": "test tool",
            "bundle": "support",
            "local_name": "test",
            "access_level": "read",
            "tags": [],
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object"},
                "output": {"type": "object"}
            },
            "secrets": [],
            "capabilities": {}
        });
        fs::write(
            dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
        )
        .expect("write metadata");

        let describe = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocol_version\":\"1\",\"tool_name\":\"{tool_name}\",\"supported_methods\":[\"tool/describe\",\"tool/invoke\"]}}}}"
        );
        let script =
            format!("#!/bin/sh\nwhile IFS= read -r _; do :; done\nprintf '%s\\n' '{describe}'\n");
        let bin = dir.join("tool-server");
        fs::write(&bin, script.as_bytes()).expect("write tool-server");
        let mut perms = fs::metadata(&bin).expect("stat tool-server").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).expect("chmod tool-server");
    }

    #[tokio::test]
    async fn build_dev_mode_resolver_enforce_fails_when_lockfile_missing() {
        let _guard = env_lock().lock().await;

        let temp = tempdir().expect("tempdir");
        let tool_dir = temp.path().join("tool");
        write_tool_fixture(&tool_dir, "support/test");
        set_env(
            BUILDER_EXTERNAL_TOOLS_ENV,
            tool_dir.to_str().expect("utf8 tool path"),
        );

        let missing_lockfile = temp.path().join(EXTERNAL_TOOLS_LOCKFILE_NAME);
        let result =
            build_dev_mode_resolver(None, &missing_lockfile, ExternalLockfileMode::Enforce, None)
                .await;

        remove_env(BUILDER_EXTERNAL_TOOLS_ENV);

        let err = match result {
            Ok(_) => panic!("enforce mode must fail when lockfile is missing"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("lockfile") && msg.contains("enforce"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn build_dev_mode_resolver_fails_with_entries_when_env_unset() {
        let _guard = env_lock().lock().await;

        remove_env(BUILDER_EXTERNAL_TOOLS_ENV);

        let temp = tempdir().expect("tempdir");
        let lockfile_path = temp.path().join(EXTERNAL_TOOLS_LOCKFILE_NAME);
        let lockfile = ExternalToolsLockfile {
            version: "1".to_string(),
            tools: vec![ExternalToolLockEntry {
                name: "support/test".to_string(),
                digest: "sha256:deadbeef".to_string(),
                abi_version: "1".to_string(),
                protocol_version: "1".to_string(),
                oci_ref: None,
                platform: None,
                signer: None,
                capabilities: None,
            }],
        };
        lockfile
            .write_to_path(&lockfile_path)
            .expect("write lockfile");

        let err = match build_dev_mode_resolver(
            None,
            &lockfile_path,
            ExternalLockfileMode::Permissive,
            None,
        )
        .await
        {
            Ok(_) => panic!("env unset with lockfile entries should fail with explicit error"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains(BUILDER_EXTERNAL_TOOLS_ENV) && msg.contains("external tool"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn build_dev_mode_resolver_permissive_continues_on_lockfile_parse_error() {
        let _guard = env_lock().lock().await;

        let temp = tempdir().expect("tempdir");
        let tool_dir = temp.path().join("tool");
        write_tool_fixture(&tool_dir, "support/test");
        set_env(
            BUILDER_EXTERNAL_TOOLS_ENV,
            tool_dir.to_str().expect("utf8 tool path"),
        );

        let bad_lockfile = temp.path().join(EXTERNAL_TOOLS_LOCKFILE_NAME);
        fs::write(&bad_lockfile, b"{not-json").expect("write malformed lockfile");

        let resolver =
            build_dev_mode_resolver(None, &bad_lockfile, ExternalLockfileMode::Permissive, None)
                .await
                .expect("permissive mode should continue with malformed lockfile");
        remove_env(BUILDER_EXTERNAL_TOOLS_ENV);

        assert!(resolver.is_some(), "resolver should still be constructed");
    }
}
