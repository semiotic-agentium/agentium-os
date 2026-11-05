// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
use baml_rt_api::RuntimeProgressMeter;
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentLister, AgentManifest,
    AgentPackageName, ArtifactHash, BamlRtError, DeploymentContentHash, ReachabilityError, Result,
    bus::BusStream,
    context::{InvocationScope, generate_context_id},
    ids::{AgentId, ContextId},
};
use baml_rt_hash::AgentIdentityDigest;
use baml_rt_mcp::{
    composite::CompositeResolver, importer::SecretResolver as McpSecretResolver,
    resolver::default_mcp_resolver_for_agent, service::McpServiceClient,
};
use baml_rt_observability::spans;
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, index_tools};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge, SecretResolverToLlmAdapter};
#[cfg(any(feature = "sandbox-provider", test))]
use baml_rt_tools::external_tools::host::{DnsResolver, PoolPolicy, VmPool};
use baml_rt_tools::{
    BundleRegistrar, ExternalToolResolver, ManifestToolNames, SharedContextRefStore,
    ToolAccessPolicy, ToolRegistry, external_tool_cache,
    external_tools::{
        EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalLifecycleEvent, ExternalLifecycleRecorder,
        ExternalRegistryResolver, ExternalToolsLockfile, ServiceHost, ToolDelivery,
        host::{HostPolicy, ToolHost, ToolPolicy, TrustDomain},
        host_native_runtime_image,
        runtime::{ToolRuntime, TrustedLaunch},
    },
    register_manifest_tools_with_fallback,
};

/// Bridges the LLM-side `SecretResolver` (returns `SecretValue`) into the
/// simple `String`-valued resolver the MCP importer/runtime expects.
struct LlmSecretResolverToMcpAdapter {
    inner: Arc<dyn baml_rt_llm_config::SecretResolver>,
}

impl LlmSecretResolverToMcpAdapter {
    fn new(inner: Arc<dyn baml_rt_llm_config::SecretResolver>) -> Self {
        Self { inner }
    }
}

impl McpSecretResolver for LlmSecretResolverToMcpAdapter {
    fn resolve(&self, name: &str) -> Option<String> {
        // Try the secret as-is first, then fall back to process env so an
        // operator with raw env vars sees them resolved without needing a
        // fnox entry per MCP secret.
        if let Some(value) = self.inner.resolve(name) {
            return Some(value.into_string());
        }
        std::env::var(name).ok()
    }
}
use baml_rt_tools_claude::{AgentWorkspaceRegistry, ClaudeSessionBundle};
use baml_tools_system::SystemBundle;
use serde_json::Value;
use tracing::{Instrument, error, info, warn};

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
    tool_host: Option<Arc<ToolHost>>,
}

pub(crate) struct JsInitialized {
    pub(crate) runtime_manager: Arc<tokio::sync::RwLock<BamlRuntimeManager>>,
    pub(crate) agent: A2aAgent,
    pub(crate) tool_host: Option<Arc<ToolHost>>,
}

/// Inputs for [`AgentPackage::boot`], grouped to satisfy clippy `too_many_arguments`.
pub(crate) struct AgentPackageBootArgs<'a> {
    /// Stable identity derived from the extracted package before bridge construction.
    pub(crate) agent_id: AgentId,
    /// Exact artifact bytes being booted. Production callers must provide the verified hash.
    pub(crate) artifact_hash: ArtifactHash,
    /// Identity digest recomputed from this package extraction.
    pub(crate) identity_digest: AgentIdentityDigest,
    /// Canonical repository content hash for this deployment.
    pub(crate) deployment_content_hash: DeploymentContentHash,
    /// Repository version embedded in the package manifest.
    pub(crate) repository_version: u32,
    pub(crate) shared_context_ref_store: SharedContextRefStore,
    pub(crate) provenance_config: &'a ProvenanceConfig,
    pub(crate) policy: &'a ToolAccessPolicy,
    pub(crate) agent_list_catalogue: Arc<dyn AgentLister>,
    pub(crate) a2a_handler: Arc<dyn A2aRequestHandler>,
    pub(crate) stream_idle_secs: Option<u64>,
    pub(crate) claude_workspaces_base: Option<&'a Path>,
    /// Meter to register the agent's JS-event-loop progress probe with.
    pub(crate) runtime_progress: Arc<RuntimeProgressMeter>,
    pub(crate) observation_notify:
        Option<tokio::sync::broadcast::Sender<baml_rt_core::ObservationUpdate>>,
    pub(crate) compaction_summarizer: Arc<dyn baml_rt_provenance::ConversationCompactionSummarizer>,
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
        runtime_manager.set_function_context_requirements(self.manifest.function_context.clone());
        info!(agent = self.manifest.name, "BAML schema loaded");
        Ok(SchemaLoaded { runtime_manager })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "registration phase threads all bundle inputs; grouping would obscure the wiring"
    )]
    async fn register_tools_phase(
        &self,
        agent_id: &AgentId,
        loaded: SchemaLoaded,
        policy: &ToolAccessPolicy,
        agent_list_catalogue: Arc<dyn AgentLister>,
        a2a_handler: Arc<dyn A2aRequestHandler>,
        provenance_store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        claude_workspaces_base: Option<&std::path::Path>,
        llm_secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver>,
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

        let lifecycle_writer: Arc<dyn ProvenanceWriter> = provenance_store.clone();
        let lifecycle_recorder = build_external_lifecycle_recorder(lifecycle_writer);
        let lockfile_path = self.extract_dir.join(EXTERNAL_TOOLS_LOCKFILE_NAME);

        // PR5 wiring: chain the dev-mode external resolver with the MCP
        // resolver via CompositeResolver so manifest entries like
        // `mcp/grafana/search_dashboards` bind to live MCP child processes.
        //
        // Snapshots are sourced **only** from the package's `mcp/` directory,
        // which is projected from the registry at build time. There is no
        // `$HOME`-level snapshot fallback: a package without `mcp/` simply
        // skips wiring the MCP resolver, and a manifest that lists `mcp/...`
        // tools then fails closed in `register_manifest_tools_with_fallback`.
        let package_mcp_root = self.extract_dir.join("mcp");
        let mcp_resolver = if package_mcp_root.exists() {
            // Pool keys are scoped to the agent package name, so two agents
            // backed by different packages never share an MCP connection
            // entry even when they resolve the same `server_id`.
            Some(default_mcp_resolver_for_agent(
                self.manifest.name.clone(),
                &package_mcp_root,
                LlmSecretResolverToMcpAdapter::new(llm_secret_resolver),
            )?)
        } else {
            None
        };
        let package_external_root = self.extract_dir.join("external-tools");
        let has_packaged_external_snapshots = package_external_root.exists();
        let packaged_snapshots = if has_packaged_external_snapshots {
            external_tool_cache::read_approved_snapshots(&self.extract_dir)?
        } else {
            Vec::new()
        };
        // Every external tool is a v2 toolset now (bundled or Service). A
        // package built for the removed v1 process/adapter runtime fails here
        // with a rebuild hint instead of as an unresolved manifest tool.
        if let Some(legacy) = packaged_snapshots.iter().find(|s| s.v2.is_none()) {
            return Err(BamlRtError::InvalidArgument(format!(
                "external tool '{}' was packaged for the removed v1 runtime; rebuild it with `karak tool build` and republish the agent",
                legacy.tool.name
            )));
        }
        if !packaged_snapshots.is_empty() {
            let lockfile = ExternalToolsLockfile::read_from_path(&lockfile_path)?;
            for snapshot in &packaged_snapshots {
                lockfile.verify_snapshot(snapshot)?;
            }
            verify_deploy_reachability(
                &manifest_tool_names,
                &lockfile,
                &packaged_snapshots,
                lifecycle_recorder.clone(),
            )
            .await?;
        }
        let mut bundled_v2_resolver = None;
        let mut tool_host = None;
        if has_packaged_external_snapshots {
            let mut bundles = Vec::new();
            for snapshot in &packaged_snapshots {
                if !matches!(
                    snapshot.v2.as_ref().map(|v2| &v2.delivery),
                    Some(ToolDelivery::Bundled { .. })
                ) {
                    continue;
                }
                let bytes = external_tool_cache::read_approved_bundle(
                    &self.extract_dir,
                    &snapshot.tool.name,
                )?;
                bundles.push((snapshot.clone(), Arc::<[u8]>::from(bytes)));
            }
            if !bundles.is_empty() {
                let host = build_v2_tool_host(agent_id, &bundles).await?;
                tool_host = Some(host.clone());
                bundled_v2_resolver =
                    Some(ExternalRegistryResolver::from_v2_bundles(host, bundles)?);
            }
        }
        let mut composite = CompositeResolver::new();
        if has_packaged_external_snapshots {
            for snapshot in packaged_snapshots.into_iter().filter(|snapshot| {
                matches!(
                    snapshot.v2.as_ref().map(|v2| &v2.delivery),
                    Some(ToolDelivery::Reference { .. })
                )
            }) {
                let v2 = snapshot.v2.as_ref().ok_or_else(|| {
                    BamlRtError::InvalidArgument("missing Service identity".into())
                })?;
                let (endpoint, source) = match &v2.delivery {
                    ToolDelivery::Reference {
                        endpoint,
                        descriptor_source,
                    } => (endpoint.clone(), descriptor_source),
                    ToolDelivery::Bundled { .. } => continue,
                };
                let client = Arc::new(McpServiceClient::from_http(endpoint, source)?);
                let host = Arc::new(
                    ServiceHost::new(
                        client,
                        std::time::Duration::from_secs(30),
                        std::time::Duration::from_secs(120),
                    )?
                    .with_lifecycle_recorder(lifecycle_recorder.clone()),
                );
                let resolver = ExternalRegistryResolver::from_v2_services(host, vec![snapshot])?;
                composite.push(Box::new(resolver) as Box<dyn ExternalToolResolver>);
            }
        }
        if let Some(registry) = bundled_v2_resolver {
            composite.push(Box::new(registry) as Box<dyn ExternalToolResolver>);
        }
        if let Some(mcp) = mcp_resolver {
            composite.push(Box::new(mcp) as Box<dyn ExternalToolResolver>);
        }
        let composite_ref: Option<&dyn ExternalToolResolver> = if composite.is_empty() {
            None
        } else {
            Some(&composite)
        };
        register_manifest_tools_with_fallback(
            runtime_manager.tool_registry().as_ref(),
            &manifest_tool_names,
            policy,
            composite_ref,
        )?;
        runtime_manager.rebuild_function_tool_manifest();
        runtime_manager
            .set_tool_allowlist(
                self.manifest
                    .tools
                    .iter()
                    // Strip any `@<semver-req>` suffix (R1 §R1.6): the allowlist
                    // keys on tool identity, which excludes the version.
                    .map(|entry| {
                        baml_rt_tools::split_manifest_tool_entry(entry)
                            .0
                            .to_string()
                    })
                    .collect::<HashSet<_>>(),
            )
            .await?;

        info!(
            agent = %self.manifest.name,
            manifest_tool_count = manifest_tool_names.len(),
            "manifest tools registered; building A2a runtime next"
        );

        Ok(ToolsRegistered {
            runtime_manager,
            tool_host,
        })
    }

    async fn build_agent_phase(
        &self,
        registered: ToolsRegistered,
        boot_event_id: baml_rt_core::ids::ActivityAnchorId,
        args: &AgentPackageBootArgs<'_>,
    ) -> Result<JsInitialized> {
        use baml_rt_quickjs::QuickJSConfig;

        let ToolsRegistered {
            mut runtime_manager,
            tool_host,
        } = registered;
        runtime_manager
            .tool_registry()
            .set_config_resolver(Some(args.provenance_config.config_service()));
        runtime_manager
            .tool_registry()
            .set_secret_resolver(Some(Arc::new(
                baml_rt_llm_config::SecretResolverToToolAdapter::new(
                    args.provenance_config.llm_secret_resolver(),
                ),
            )));
        runtime_manager.set_llm_secret_resolver(Arc::new(SecretResolverToLlmAdapter::new(
            args.provenance_config.llm_secret_resolver(),
        )));

        let llm_config_arc = {
            use baml_rt_llm_config::{LLM_CONFIG_BUNDLE_NAME, LlmClientConfig, StaticResolver};
            let config_service = args.provenance_config.config_service();
            let bundle = baml_rt_tools::BundleName::new(LLM_CONFIG_BUNDLE_NAME)
                .expect("llm bundle name valid");
            let stored_llm_config = match config_service.get(&bundle).await {
                Ok(Some(v)) => match LlmClientConfig::from_value(v) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::warn!(error = %e, "stored LLM config parse failed; using platform fallback only");
                        None
                    }
                },
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load LLM config; using platform fallback only");
                    None
                }
            };

            let agent_package = Some(self.manifest.name.as_str());
            match stored_llm_config {
                Some(llm_config) => {
                    tracing::info!(
                        default = %llm_config.default,
                        clients = llm_config.clients.len(),
                        agent_overrides = llm_config.overrides.agent.len(),
                        function_overrides = llm_config.overrides.agent_function.len(),
                        "stored LLM client config loaded for override resolution"
                    );
                    let llm_config_arc = Arc::new(llm_config);
                    let resolver = Arc::new(StaticResolver::with_agent_package(
                        Arc::clone(&llm_config_arc),
                        args.provenance_config.llm_secret_resolver(),
                        agent_package,
                    ));
                    runtime_manager.set_llm_client_resolver(resolver);
                    runtime_manager.set_agent_package(agent_package);
                    llm_config_arc
                }
                None => {
                    let fallback_config = LlmClientConfig::sensible_default();
                    tracing::info!(
                        default = %fallback_config.default,
                        clients = fallback_config.clients.len(),
                        "no stored LLM client config; platform fallback will apply only to functions without BAML client config"
                    );
                    let fallback_config_arc = Arc::new(fallback_config);
                    let fallback_resolver = Arc::new(StaticResolver::with_agent_package(
                        Arc::clone(&fallback_config_arc),
                        args.provenance_config.llm_secret_resolver(),
                        agent_package,
                    ));
                    runtime_manager.set_llm_fallback_client_resolver(fallback_resolver);
                    runtime_manager.set_agent_package(agent_package);
                    fallback_config_arc
                }
            }
        };

        let runtime_manager_arc = Arc::new(tokio::sync::RwLock::new(runtime_manager));
        let store = args.provenance_config.store().clone();
        let host_ingress_recorder =
            Arc::new(crate::services::HostIngressRecorderImpl::new(store.clone()));
        let quickjs_config = QuickJSConfig::new()
            .with_stream_collector_idle_secs(args.stream_idle_secs)
            .with_host_ingress_recorder(Some(host_ingress_recorder));
        let agent_package = AgentPackageName::parse(&self.manifest.name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "manifest agent name '{name}' is not a valid agent_package identifier",
                name = self.manifest.name
            ))
        })?;
        let mut agent_builder = A2aAgent::builder()
            .with_agent_id(args.agent_id.clone())
            .with_runtime_handle(runtime_manager_arc.clone())
            .with_quickjs_config(quickjs_config)
            .with_baml_helpers(true)
            .with_agent_identity(agent_package)
            .with_boot_event_id(boot_event_id)
            .with_surreal_store(store)
            .with_llm_client_config(llm_config_arc);
        if let Some(tx) = args.observation_notify.clone() {
            agent_builder = agent_builder.with_observation_notify(tx);
        }
        let agent_builder =
            agent_builder.with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()));
        let agent_builder =
            agent_builder.with_compaction_summarizer(Arc::clone(&args.compaction_summarizer));
        info!(
            agent = %self.manifest.name,
            "building QuickJS runtime and A2a bridge (often the longest boot step)"
        );
        let agent = agent_builder.build().await?;
        info!(agent = %self.manifest.name, "QuickJS runtime and A2a bridge ready");
        Ok(JsInitialized {
            runtime_manager: runtime_manager_arc,
            agent,
            tool_host,
        })
    }

    async fn initialize_js_phase(&self, built: JsInitialized) -> Result<JsInitialized> {
        let entry_point_path = self.extract_dir.join(&self.manifest.entry_point);
        let agent_code = match tokio::fs::read_to_string(&entry_point_path).await {
            Ok(agent_code) => agent_code,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(
                    entry_point = self.manifest.entry_point,
                    "Agent entry point not found, skipping JavaScript initialization"
                );
                return Ok(built);
            }
            Err(e) => return Err(BamlRtError::Io(e)),
        };
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
        Ok(built)
    }

    pub(crate) async fn boot(
        &self,
        args: AgentPackageBootArgs<'_>,
    ) -> Result<(
        A2aAgent,
        AgentId,
        baml_rt_core::ids::ActivityAnchorId,
        Option<Arc<ToolHost>>,
    )> {
        async {
            info!(
                agent = %self.manifest.name,
                extract_dir = %self.extract_dir.display(),
                "agent package boot starting"
            );
            let agent_type_parsed =
                AgentType::new(self.manifest.name.clone()).ok_or_else(|| {
                    BamlRtError::InvalidArgument("agent_type cannot be empty".to_string())
                })?;
            let boot_event = ProvEvent::agent_booted_with_identity(
                args.agent_id.clone(),
                agent_type_parsed,
                args.repository_version.to_string(),
                args.identity_digest.to_string(),
                args.artifact_hash.to_string(),
                args.deployment_content_hash.as_str().to_string(),
                baml_rt_observability::service_instance_id().to_string(),
            );
            let boot_event_id = boot_event.id().clone();
            let loaded = self
                .load_schema_phase(
                    args.shared_context_ref_store.clone(),
                    args.provenance_config,
                )
                .await?;
            let registered = self
                .register_tools_phase(
                    &args.agent_id,
                    loaded,
                    args.policy,
                    Arc::clone(&args.agent_list_catalogue),
                    Arc::clone(&args.a2a_handler),
                    args.provenance_config.store().clone(),
                    args.claude_workspaces_base,
                    args.provenance_config.llm_secret_resolver(),
                )
                .await?;
            let built = self
                .build_agent_phase(registered, boot_event_id.clone(), &args)
                .await?;
            // Register before `initialize_js_phase`: a CPU-bound JS top-level
            // is the worst-case peg the meter must observe, and it runs inside
            // that phase. The block scope is load-bearing: it drops
            // `bridge_guard` before `initialize_js_phase` re-locks the bridge.
            {
                let bridge_arc = built.agent.bridge();
                let mut bridge_guard = bridge_arc.lock().await;
                bridge_guard.register_progress_probe(args.runtime_progress.as_ref());
            }
            let initialized = self.initialize_js_phase(built).await?;
            let JsInitialized {
                agent,
                runtime_manager: runtime_manager_arc,
                tool_host,
            } = initialized;

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
            if agent_id != args.agent_id {
                return Err(BamlRtError::InvalidArgument(format!(
                    "agent bridge identity mismatch: expected {}, built {}",
                    args.agent_id, agent_id
                )));
            }
            let writer = args.provenance_config.store().clone() as Arc<dyn ProvenanceWriter>;
            if let Err(e) = writer.add_event(boot_event).await {
                error!(error = ?e, agent_id = %agent_id, "Failed to write AgentBooted event");
            } else {
                info!(agent_id = %agent_id, "AgentBooted event written to provenance store");
            }

            Ok((agent, agent_id, boot_event_id, tool_host))
        }
        .instrument(spans::load_agent_package(&self.extract_dir))
        .await
    }

    pub(crate) fn name(&self) -> &str {
        &self.manifest.name
    }

    pub(crate) fn manifest(&self) -> &AgentManifest {
        &self.manifest
    }

    /// Derive the stable identity digest from this fresh packaged-artifact extraction.
    pub(crate) fn identity_digest(&self) -> Result<AgentIdentityDigest> {
        let entries =
            baml_rt_repository::identity_entries_from_extracted_package(&self.extract_dir)
                .map_err(|error| {
                    BamlRtError::InvalidArgument(format!(
                        "failed to collect packaged agent identity entries: {error}"
                    ))
                })?;
        baml_rt_hash::compute_agent_identity_digest(&entries).map_err(|error| {
            BamlRtError::InvalidArgument(format!(
                "failed to compute packaged agent identity digest: {error}"
            ))
        })
    }

    /// Derive the stable agent ID before constructing the bridge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn derived_agent_id(&self) -> Result<AgentId> {
        Ok(AgentId::from_identity_digest(&self.identity_digest()?))
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
        artifact_hash: ArtifactHash,
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
    pub(crate) boot_event_id: baml_rt_core::ids::ActivityAnchorId,
    /// Host authority for releasing this agent's pooled tool VMs on undeploy.
    pub(crate) tool_host: Option<Arc<ToolHost>>,
}

impl BootedAgent {
    pub(crate) fn content_hash(&self) -> Option<&DeploymentContentHash> {
        match &self.provenance {
            DeploymentProvenance::Repository { content_hash, .. } => Some(content_hash),
            DeploymentProvenance::Ephemeral => None,
        }
    }

    pub(crate) fn artifact_hash(&self) -> Option<&ArtifactHash> {
        match &self.provenance {
            DeploymentProvenance::Repository { artifact_hash, .. } => Some(artifact_hash),
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
// External tool resolver (allowed source dirs + approved snapshots).
// ---------------------------------------------------------------------------

fn build_external_lifecycle_recorder(
    writer: Arc<dyn ProvenanceWriter>,
) -> ExternalLifecycleRecorder {
    Arc::new(move |event| {
        // Allocate the event anchor before spawning the write. This preserves
        // emission order even when independent writes complete out of order.
        let event = external_lifecycle_provenance_event(event);
        let writer = writer.clone();
        tokio::spawn(async move {
            if let Err(e) = writer.add_event(event).await {
                tracing::warn!(error = ?e, "failed to record external tool lifecycle provenance event");
            }
        });
    })
}

fn external_lifecycle_provenance_event(event: ExternalLifecycleEvent) -> ProvEvent {
    let (tool_name, phase, result, details, context_id) = match event {
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
            None,
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
            None,
        ),
        ExternalLifecycleEvent::SchemaDrift {
            tool_name,
            expected_schema_digest,
            observed_schema_digest,
        } => (
            tool_name,
            "schema_drift".to_string(),
            "drifted".to_string(),
            serde_json::json!({
                "expected_schema_digest": expected_schema_digest,
                "observed_schema_digest": observed_schema_digest,
            }),
            None,
        ),
        ExternalLifecycleEvent::TrustGranted {
            tool_name,
            agent_id,
            tool_digest,
            node,
        } => (
            tool_name,
            "trust".to_string(),
            "granted".to_string(),
            serde_json::json!({
                "agent_id": agent_id,
                "tool_digest": tool_digest,
                "node": node,
            }),
            None,
        ),
        ExternalLifecycleEvent::TrustRevoked {
            tool_name,
            agent_id,
            tool_digest,
            node,
        } => (
            tool_name,
            "trust".to_string(),
            "revoked".to_string(),
            serde_json::json!({
                "agent_id": agent_id,
                "tool_digest": tool_digest,
                "node": node,
            }),
            None,
        ),
        ExternalLifecycleEvent::TrustedInvoke {
            tool_name,
            agent_id,
            tool_digest,
            node,
            artifact_hash,
            context_id,
            task_id,
            invocation_id,
            trace_id,
        } => {
            let event_context =
                (!context_id.is_empty()).then(|| ContextId::from(context_id.as_str()));
            (
                tool_name,
                "trusted_invoke".to_string(),
                "operator_trusted".to_string(),
                serde_json::json!({
                    "agent_id": agent_id,
                    "tool_digest": tool_digest,
                    "node": node,
                    "artifact_hash": artifact_hash,
                    "context_id": context_id,
                    "task_id": task_id,
                    "invocation_id": invocation_id,
                    "trace_id": trace_id,
                }),
                event_context,
            )
        }
        ExternalLifecycleEvent::ServiceBound {
            tool_name,
            endpoint,
        } => (
            tool_name,
            "service".to_string(),
            "bound".to_string(),
            serde_json::json!({
                "endpoint": endpoint,
            }),
            None,
        ),
    };

    ProvEvent::external_tool_lifecycle(
        context_id.unwrap_or_else(generate_context_id),
        tool_name,
        phase,
        result,
        details,
    )
}

/// Runner identity owning this process's tool VMs. `karak serve` installs the
/// identity persisted under `--state-dir`; library hosts that never install
/// one get a fresh per-process id (no leftover reclaim across restarts).
static TOOL_RUNNER_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// One VM pool for the whole runner: every agent's `ToolHost` shares the VM
/// budget, and VMs outlive individual agent loads (idle eviction reclaims them).
#[cfg(feature = "sandbox-provider")]
static TOOL_VM_POOL: tokio::sync::OnceCell<Arc<VmPool>> = tokio::sync::OnceCell::const_new();

/// Install the runner identity that owns tool VMs. First call wins.
pub(crate) fn install_tool_runner_id(runner_id: String) {
    if TOOL_RUNNER_ID.set(runner_id).is_err() {
        warn!("tool runner id already installed; keeping the first");
    }
}

#[cfg(feature = "sandbox-provider")]
fn tool_runner_id() -> String {
    TOOL_RUNNER_ID
        .get_or_init(baml_rt_tools::external_tools::sandbox::fresh_runner_id)
        .clone()
}

/// Tear down every tool VM this process started. Called on runner exit.
pub(crate) async fn shutdown_tool_vms() {
    #[cfg(feature = "sandbox-provider")]
    if let Some(pool) = TOOL_VM_POOL.get() {
        match pool.shutdown().await {
            Ok(()) => info!(runner_id = pool.runner_id(), "tool VMs torn down"),
            Err(error) => warn!(%error, "tool VM teardown failed"),
        }
    }
}

#[cfg(feature = "sandbox-provider")]
async fn runner_tool_vm_pool() -> Result<Arc<VmPool>> {
    use baml_rt_tools::external_tools::sandbox::{MicrosandboxProvider, SandboxProvider};
    TOOL_VM_POOL
        .get_or_try_init(|| async {
            let runtime_image = baml_rt_tools::external_tools::runtime_image_for_tier(
                baml_rt_tools::external_tools::KtoolRuntimeTier::Sandbox,
            );
            let image_ref = std::env::var("KARAKARA_KVM_IMAGE").map_err(|_| {
                BamlRtError::InvalidArgument(
                    "bundled v2 tools require KARAKARA_KVM_IMAGE set to a digest-pinned OCI image"
                        .into(),
                )
            })?;
            let provider =
                MicrosandboxProvider::new()?.with_runtime_image(runtime_image, image_ref)?;
            let provider: Arc<dyn SandboxProvider> = Arc::new(provider);
            Ok(VmPool::start(provider, tool_runner_id(), PoolPolicy::default()).await)
        })
        .await
        .cloned()
}

/// Test-only provider for builds without `sandbox-provider`: registration
/// works, booting a VM fails closed.
#[cfg(all(not(feature = "sandbox-provider"), test))]
struct NoVmProvider;

#[cfg(all(not(feature = "sandbox-provider"), test))]
#[async_trait::async_trait]
impl baml_rt_tools::external_tools::sandbox::SandboxProvider for NoVmProvider {
    async fn create_vm(
        &self,
        _spec: baml_rt_tools::external_tools::host::VmSpec,
    ) -> Result<(
        baml_rt_tools::external_tools::sandbox::SandboxHandle,
        baml_rt_tools::external_tools::sandbox::TsrpcChannel,
    )> {
        Err(BamlRtError::InvalidArgument(
            "tool VMs require the sandbox-provider feature".into(),
        ))
    }

    async fn teardown(
        &self,
        _handle: &baml_rt_tools::external_tools::sandbox::SandboxHandle,
    ) -> Result<()> {
        Ok(())
    }
}

async fn build_v2_tool_host(
    agent_id: &AgentId,
    bundles: &[(
        baml_rt_tools::external_tools::ExternalToolSnapshot,
        Arc<[u8]>,
    )],
) -> Result<Arc<ToolHost>> {
    let mut policy = HostPolicy::default();
    for (snapshot, _) in bundles {
        let Some(v2) = snapshot.v2.as_ref() else {
            continue;
        };
        let ToolDelivery::Bundled { tool_digest, .. } = &v2.delivery else {
            continue;
        };
        // Repository approval is the operator decision for the toolset's exact
        // requested capabilities. The policy key still includes the agent and
        // exact digest, so a different agent or bundle cannot reuse this grant.
        policy.tools.insert(
            (agent_id.to_string(), *tool_digest),
            ToolPolicy {
                trust_domain: TrustDomain::ThirdParty,
                capabilities: v2.requested.clone(),
                secret_hosts: std::collections::BTreeMap::new(),
            },
        );
    }

    #[cfg(feature = "sandbox-provider")]
    {
        let pool = runner_tool_vm_pool().await?;
        return Ok(Arc::new(ToolHost::new(
            pool,
            policy,
            Arc::new(DnsResolver),
        )?));
    }

    #[cfg(all(not(feature = "sandbox-provider"), test))]
    {
        use baml_rt_tools::external_tools::sandbox::{SandboxProvider, fresh_runner_id};
        // Package wiring and registration run; an actual invoke fails closed.
        let provider: Arc<dyn SandboxProvider> = Arc::new(NoVmProvider);
        // Tests keep a pool per host so parallel tests never share a VM budget.
        let pool = VmPool::new(provider, fresh_runner_id(), PoolPolicy::default());
        return Ok(Arc::new(ToolHost::new(
            pool,
            policy,
            Arc::new(DnsResolver),
        )?));
    }

    #[cfg(all(not(feature = "sandbox-provider"), not(test)))]
    {
        let _ = (agent_id, bundles, policy);
        Err(BamlRtError::InvalidArgument(
            "bundled v2 tools require the sandbox-provider feature".into(),
        ))
    }
}

/// Verify placement constraints for the exact approved snapshots selected by
/// lockfile v2. A failed check stops deployment here. It must not trigger a
/// build or select a different node or endpoint.
async fn verify_deploy_reachability(
    manifest_tools: &ManifestToolNames,
    lockfile: &ExternalToolsLockfile,
    snapshots: &[baml_rt_tools::external_tools::ExternalToolSnapshot],
    lifecycle_recorder: ExternalLifecycleRecorder,
) -> Result<()> {
    let local_node = std::env::var("KARAKARA_NODE_ID")
        .ok()
        .filter(|node| !node.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());
    let mut checked_toolsets = HashSet::new();

    for name in manifest_tools.iter() {
        let Some(toolset) = lockfile.toolset_for(name) else {
            // Static tools are not external-tool lockfile entries.
            continue;
        };
        let snapshot = lockfile.select_approved_snapshot(name, snapshots)?;
        let tool_digest = snapshot
            .tool_digest()
            .map(|digest| digest.to_string())
            .unwrap_or_else(|| toolset.tool_digest.clone());
        // Resolve every used interface pin, then probe each toolset only once.
        // ServiceHost::verify already checks every descriptor in its toolset.
        if !checked_toolsets.insert((toolset.name.clone(), tool_digest.clone())) {
            continue;
        }

        match toolset.tier.as_str() {
            // Snapshot validation above remains the Sandbox image-presence
            // check. Sandbox placement has no extra node constraint.
            "sandbox" => {}
            "trusted" => {
                let is_trusted_bundle = snapshot.v2.as_ref().is_some_and(|v2| {
                    matches!(
                        &v2.delivery,
                        ToolDelivery::Bundled { runtime_image, .. }
                            if *runtime_image == host_native_runtime_image()
                    )
                });
                if !is_trusted_bundle {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "trusted lockfile entry '{}' does not resolve to a trusted bundle",
                        name
                    )));
                }
                verify_trusted_reachability(snapshot, &tool_digest, &local_node)?;
            }
            "service" => {
                verify_service_reachability(snapshot, &tool_digest, lifecycle_recorder.clone())
                    .await?
            }
            tier => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool '{}' has unsupported lockfile tier '{tier}'",
                    name
                )));
            }
        }
    }
    Ok(())
}

fn verify_trusted_reachability(
    snapshot: &baml_rt_tools::external_tools::ExternalToolSnapshot,
    tool_digest: &str,
    local_node: &str,
) -> Result<()> {
    // Bundled Trusted toolsets from P5.3 have no separate launch declaration:
    // their host supervisor is installed by the runner. When an operator adds
    // a Trusted runtime declaration, it is an explicit placement contract and
    // both launch modes are checked here.
    let Some(ToolRuntime::Trusted(runtime)) = snapshot.tool.runtime.as_ref() else {
        return Ok(());
    };
    let required_node = runtime.node.as_deref().unwrap_or("local");
    if required_node != local_node {
        return Err(BamlRtError::Reachability(Box::new(
            ReachabilityError::node(&snapshot.tool.name, tool_digest, required_node),
        )));
    }

    let reachable = match &runtime.launch {
        // An empty argv is a bundle-declared placement with no host program: the
        // runner spawns its own installed supervisor. Only a declared program
        // can be probed from here.
        TrustedLaunch::Spawn { argv, .. } => argv
            .first()
            .is_none_or(|program| trusted_spawn_is_installed(program)),
        TrustedLaunch::Attach { socket_path } => trusted_attach_socket_exists(socket_path),
    };
    if reachable {
        Ok(())
    } else {
        Err(BamlRtError::Reachability(Box::new(
            ReachabilityError::node(&snapshot.tool.name, tool_digest, required_node),
        )))
    }
}

fn trusted_spawn_is_installed(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

fn trusted_attach_socket_exists(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        path.metadata()
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

async fn verify_service_reachability(
    snapshot: &baml_rt_tools::external_tools::ExternalToolSnapshot,
    tool_digest: &str,
    lifecycle_recorder: ExternalLifecycleRecorder,
) -> Result<()> {
    let v2 = snapshot.v2.as_ref().ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "service tool '{}' is missing v2 delivery metadata",
            snapshot.tool.name
        ))
    })?;
    let (endpoint, source) = match &v2.delivery {
        ToolDelivery::Reference {
            endpoint,
            descriptor_source,
        } => (endpoint.clone(), descriptor_source),
        ToolDelivery::Bundled { .. } => {
            return Err(BamlRtError::InvalidArgument(format!(
                "service tool '{}' has bundled delivery metadata",
                snapshot.tool.name
            )));
        }
    };
    let endpoint_name = match &endpoint {
        baml_rt_tools::external_tools::ServiceEndpoint::Mcp { url } => url.clone(),
        _ => format!("{endpoint:?}"),
    };
    let result = async {
        let client = Arc::new(McpServiceClient::from_http(endpoint, source)?);
        let host = ServiceHost::new(
            client,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(120),
        )?
        .with_lifecycle_recorder(lifecycle_recorder);
        host.verify(snapshot).await
    }
    .await;
    result.map_err(|error| {
        BamlRtError::Reachability(Box::new(ReachabilityError::endpoint(
            &snapshot.tool.name,
            tool_digest,
            endpoint_name,
            error,
        )))
    })
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
    use std::{fs, path::Path};

    use baml_rt_tools::{
        external_tools::{
            CapabilityRequest, DescribeProvenance, ErrorClass, ExternalLifecycleEvent,
            ExternalToolInterfaceLockEntry, ExternalToolSnapshot, ExternalToolsLockfile,
            ExternalToolsetLockEntry, KtoolRuntimeTier, OutputTrust, ServiceEndpoint, SideEffect,
            ToolRuntime, TrustedLaunch, TrustedRuntimeSpec, UnifiedToolDescriptor,
            runtime_image_for_tier,
        },
        mcp_snapshot::canonical_digest,
    };
    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        AgentPackage, ExternalLifecycleRecorder, ManifestToolNames,
        external_lifecycle_provenance_event, verify_deploy_reachability,
    };

    fn packaged_identity_fixture(root: &Path, source: &str) -> AgentPackage {
        let baml_src = root.join("baml_src");
        fs::create_dir_all(root.join("src")).expect("create source directory");
        fs::create_dir_all(root.join("dist")).expect("create output directory");
        fs::create_dir_all(&baml_src).expect("create BAML directory");
        fs::write(
            root.join("manifest.json"),
            r#"{"name":"stable-agent","version":"1","entry_point":"dist/index.js"}"#,
        )
        .expect("write manifest");
        fs::write(root.join("src/index.ts"), source).expect("write source");
        fs::write(root.join("dist/index.js"), "compiled output").expect("write entry point");
        fs::write(baml_src.join("agent.baml"), "function Run() -> string").expect("write BAML");
        let manifest =
            serde_json::from_slice(&fs::read(root.join("manifest.json")).expect("read manifest"))
                .expect("parse manifest");
        AgentPackage {
            manifest,
            extract_dir: root.to_path_buf(),
            baml_src,
        }
    }

    #[test]
    fn boot_derives_stable_agent_id() {
        let temp = tempdir().expect("tempdir");
        let first =
            packaged_identity_fixture(&temp.path().join("first"), "export const run = 1;\n");
        let second =
            packaged_identity_fixture(&temp.path().join("second"), "export const run = 1;\n");
        let changed =
            packaged_identity_fixture(&temp.path().join("changed"), "export const run = 2;\n");

        assert_eq!(
            first.derived_agent_id().expect("derive first ID"),
            second.derived_agent_id().expect("derive second ID"),
        );
        assert_ne!(
            first.derived_agent_id().expect("derive first ID"),
            changed.derived_agent_id().expect("derive changed ID"),
        );
    }

    #[test]
    fn trusted_invoke_provenance_keeps_explicit_call_identity() {
        let event = external_lifecycle_provenance_event(ExternalLifecycleEvent::TrustedInvoke {
            tool_name: "support/gpu".into(),
            agent_id: "agent-1".into(),
            tool_digest: "sha256:tool".into(),
            node: "gpu-1".into(),
            artifact_hash: "sha256:artifact".into(),
            context_id: "ctx-audit".into(),
            task_id: "task-1".into(),
            invocation_id: "invoke-1".into(),
            trace_id: "trace-1".into(),
        });

        assert_eq!(event.context_id().as_str(), "ctx-audit");
        let baml_rt_provenance::ProvEventData::ExternalToolLifecycle {
            tool_name,
            phase,
            result,
            details,
        } = event.data()
        else {
            panic!("expected external tool lifecycle provenance");
        };
        assert_eq!(tool_name, "support/gpu");
        assert_eq!(phase, "trusted_invoke");
        assert_eq!(result, "operator_trusted");
        assert_eq!(details["agent_id"], "agent-1");
        assert_eq!(details["tool_digest"], "sha256:tool");
        assert_eq!(details["node"], "gpu-1");
        assert_eq!(details["artifact_hash"], "sha256:artifact");
        assert_eq!(details["context_id"], "ctx-audit");
        assert_eq!(details["task_id"], "task-1");
        assert_eq!(details["invocation_id"], "invoke-1");
        assert_eq!(details["trace_id"], "trace-1");
    }

    fn trusted_snapshot(launch: TrustedLaunch, node: Option<&str>) -> ExternalToolSnapshot {
        let descriptor = UnifiedToolDescriptor {
            descriptor_abi: 1,
            name: "support/trusted".into(),
            version: "1.0.0".into(),
            description: "trusted reachability fixture".into(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            error_classes: vec![ErrorClass::Execution],
            side_effect: SideEffect::Read,
            idempotent: true,
            output_trust: OutputTrust::Untrusted,
        };
        let bundle_digest = canonical_digest(&json!("bundle"));
        let tool_digest = canonical_digest(&json!("tool"));
        let mut snapshot = ExternalToolSnapshot::from_v2_bundle(
            descriptor,
            CapabilityRequest::default(),
            bundle_digest,
            tool_digest,
            runtime_image_for_tier(KtoolRuntimeTier::Trusted),
            "operator",
            "2026-01-01T00:00:00Z",
        )
        .expect("trusted snapshot");
        snapshot.tool.runtime = Some(ToolRuntime::Trusted(TrustedRuntimeSpec {
            launch,
            node: node.map(str::to_string),
        }));
        snapshot
    }

    fn trusted_lockfile(snapshot: &ExternalToolSnapshot) -> ExternalToolsLockfile {
        ExternalToolsLockfile {
            version: "2".to_string(),
            toolsets: vec![ExternalToolsetLockEntry {
                name: "support".to_string(),
                version: "1.0.0".to_string(),
                requirement: "*".to_string(),
                tier: "trusted".to_string(),
                tool_digest: snapshot.tool_digest().expect("tool digest").to_string(),
                tools: vec![ExternalToolInterfaceLockEntry {
                    name: "trusted".to_string(),
                    descriptor_digest: snapshot
                        .v2
                        .as_ref()
                        .expect("v2")
                        .descriptor_digest
                        .to_string(),
                }],
            }],
        }
    }

    fn lifecycle_recorder() -> ExternalLifecycleRecorder {
        std::sync::Arc::new(|_| {})
    }

    #[tokio::test]
    async fn deploy_gate_rejects_trusted_tool_on_another_node() {
        let snapshot = trusted_snapshot(
            TrustedLaunch::Spawn {
                argv: vec!["sh".into()],
                cwd: None,
                env_allowlist: Vec::new(),
            },
            Some("gpu-1"),
        );
        let lockfile = trusted_lockfile(&snapshot);
        let tools = ManifestToolNames::parse(&["support/trusted".to_string()]).unwrap();

        let error =
            verify_deploy_reachability(&tools, &lockfile, &[snapshot], lifecycle_recorder())
                .await
                .expect_err("a remote trusted node must not be silently selected");
        let baml_rt_core::BamlRtError::Reachability(error) = error else {
            panic!("expected ReachabilityError");
        };
        assert_eq!(error.tool_name, "support/trusted");
        assert_eq!(error.target_kind.to_string(), "node");
        assert_eq!(error.target, "gpu-1");
    }

    #[tokio::test]
    async fn deploy_gate_rejects_missing_trusted_spawn_argv() {
        let snapshot = trusted_snapshot(
            TrustedLaunch::Spawn {
                argv: vec!["karakara-command-that-is-not-installed".into()],
                cwd: None,
                env_allowlist: Vec::new(),
            },
            None,
        );
        let lockfile = trusted_lockfile(&snapshot);
        let tools = ManifestToolNames::parse(&["support/trusted".to_string()]).unwrap();

        let error =
            verify_deploy_reachability(&tools, &lockfile, &[snapshot], lifecycle_recorder())
                .await
                .expect_err("a missing trusted command must block deployment");
        assert!(matches!(error, baml_rt_core::BamlRtError::Reachability(_)));
    }

    #[tokio::test]
    async fn deploy_gate_rejects_missing_trusted_attach_socket() {
        let socket = tempfile::tempdir().unwrap().path().join("missing.sock");
        let snapshot = trusted_snapshot(
            TrustedLaunch::Attach {
                socket_path: socket,
            },
            None,
        );
        let lockfile = trusted_lockfile(&snapshot);
        let tools = ManifestToolNames::parse(&["support/trusted".to_string()]).unwrap();

        let error =
            verify_deploy_reachability(&tools, &lockfile, &[snapshot], lifecycle_recorder())
                .await
                .expect_err("a missing attached daemon socket must block deployment");
        let baml_rt_core::BamlRtError::Reachability(error) = error else {
            panic!("expected ReachabilityError");
        };
        assert_eq!(error.target_kind.to_string(), "node");
        assert_eq!(error.target, "local");
    }

    #[tokio::test]
    async fn deploy_gate_names_unreachable_service_endpoint() {
        let descriptor = test_support::external_tools_v2::descriptor("support/service");
        let snapshot = ExternalToolSnapshot::from_v2_service(
            descriptor,
            ServiceEndpoint::Mcp {
                url: "http://127.0.0.1:1/mcp".into(),
            },
            DescribeProvenance::McpToolsList {
                protocol_version: "2025-06-18".into(),
                server_identity_digest: canonical_digest(&json!("server")),
                tools_digest: canonical_digest(&json!(["service"])),
                tool_name: "service".into(),
            },
            "operator",
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let lockfile = ExternalToolsLockfile {
            version: "2".into(),
            toolsets: vec![ExternalToolsetLockEntry {
                name: "support".into(),
                version: "1.0.0".into(),
                requirement: "*".into(),
                tier: "service".into(),
                tool_digest: snapshot.tool_digest().unwrap().to_string(),
                tools: vec![ExternalToolInterfaceLockEntry {
                    name: "service".into(),
                    descriptor_digest: snapshot.v2.as_ref().unwrap().descriptor_digest.to_string(),
                }],
            }],
        };
        let tools = ManifestToolNames::parse(&["support/service".to_string()]).unwrap();

        let error =
            verify_deploy_reachability(&tools, &lockfile, &[snapshot], lifecycle_recorder())
                .await
                .expect_err("an invalid endpoint must block deployment");
        let baml_rt_core::BamlRtError::Reachability(error) = error else {
            panic!("expected ReachabilityError");
        };
        assert_eq!(error.tool_name, "support/service");
        assert_eq!(error.target_kind.to_string(), "endpoint");
        assert_eq!(error.target, "http://127.0.0.1:1/mcp");
    }
}
