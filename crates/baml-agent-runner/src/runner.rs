// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! AgentRunner: host lifecycle — agent map, deploy/undeploy, dispatch, stdio loop.

use std::{
    collections::HashMap,
    str::FromStr as _,
    sync::{Arc, RwLock, atomic::AtomicU8},
};

use async_trait::async_trait;
use baml_rt_a2a::{A2aRequestHandler, a2a};
use baml_rt_api::RuntimeProgressMeter;
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentInstanceId, AgentLister, AgentPackageName, AgentRouteKey,
    BamlRtError, ConversationHistoryUpdate, DeploymentContentHash, DeploymentManager,
    DeploymentRecord, DeploymentStatus, Result, UndeployResult,
    bus::BusStream,
    callback_scheduling_scopes_differ_from_dispatch, context,
    ids::{AgentId, ContextId, TaskId},
    scheduling_scope_from_dispatch_metadata,
};
use baml_rt_observability::spans;
use baml_rt_provenance::{ProvEvent, ProvenanceWriter};
use baml_rt_repository::{ContentHash, RepositoryService, manifest_package_name_from_tar_gz};
use baml_rt_tools::{SharedContextRefStore, ToolAccessPolicy};
use serde_json::Value;
use tokio::{io::AsyncWriteExt, sync::broadcast};

use crate::{
    agent_package::{
        AgentLifecycleState, AgentPackage, AgentPackageBootArgs, BootedAgent, DeploymentProvenance,
        SnapshotAgentLister,
    },
    config::ProvenanceConfig,
    routing::{InternalA2aRouter, LiveAgentLister, ScopedInternalA2aRouter, scope_from_request},
    stdio::{
        is_a2a_method, map_a2a_error, select_implicit_stdio_agent, serialize_a2a_response,
        split_agent_method, strip_stream_suffix, unix_timestamp_secs, wrap_plaintext_message,
    },
};

fn callback_dispatch_context_link_event(
    request: &AgentDispatchRequest,
    agent_id: &AgentId,
) -> Option<ProvEvent> {
    let meta = request.metadata.as_ref()?;
    let (sched_ctx, sched_task) = scheduling_scope_from_dispatch_metadata(meta)?;
    let dispatch_ctx = request.context_id.as_ref()?;
    let dispatch_task = request.task_id.as_ref()?;
    if !callback_scheduling_scopes_differ_from_dispatch(
        &sched_ctx,
        &sched_task,
        dispatch_ctx,
        dispatch_task,
    ) {
        return None;
    }
    Some(ProvEvent::callback_dispatch_contexts_linked(
        dispatch_ctx.clone(),
        sched_ctx,
        sched_task,
        dispatch_task.clone(),
        agent_id.clone(),
    ))
}

fn parse_repository_entry_version(value: &serde_json::Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return u32::try_from(n).ok();
    }
    if let Some(s) = value.as_str() {
        return s.strip_prefix('v').unwrap_or(s).parse::<u32>().ok();
    }
    None
}

pub(crate) struct AgentRunnerConfig {
    pub(crate) provenance_config: ProvenanceConfig,
    pub(crate) deployment_state: Arc<crate::deployment_state::DeploymentStateStore>,
    pub(crate) access_policy: ToolAccessPolicy,
    pub(crate) stream_idle_secs: Option<u64>,
    pub(crate) claude_workspaces_base: Option<std::path::PathBuf>,
    pub(crate) repository_url: String,
    pub(crate) embedded_repository: Option<Arc<RepositoryService>>,
    pub(crate) external_tools_dirs: Vec<std::path::PathBuf>,
    pub(crate) sandbox_bind_roots: Vec<std::path::PathBuf>,
    pub(crate) runtime_progress: Arc<RuntimeProgressMeter>,
    pub(crate) conversation_history_notify: Option<broadcast::Sender<ConversationHistoryUpdate>>,
}

/// Agent runner host: manages agents and composes the tool catalogue at startup.
pub(crate) struct AgentRunner {
    pub(crate) agents: RwLock<HashMap<String, BootedAgent>>,
    pub(crate) provenance_config: ProvenanceConfig,
    pub(crate) deployment_state: Arc<crate::deployment_state::DeploymentStateStore>,
    pub(crate) access_policy: ToolAccessPolicy,
    pub(crate) routed_agents: std::sync::RwLock<HashMap<AgentRouteKey, baml_rt_a2a::A2aAgent>>,
    pub(crate) internal_a2a_router: Arc<InternalA2aRouter>,
    pub(crate) stream_idle_secs: Option<u64>,
    pub(crate) claude_workspaces_base: Option<std::path::PathBuf>,
    pub(crate) repository_url: String,
    /// Same persistence as `GET /repository/*`; satisfies deploy/restore without depending on HTTP.
    pub(crate) embedded_repository: Option<Arc<RepositoryService>>,
    pub(crate) repository_http_client: reqwest::Client,
    /// Cluster manager for recording agent placements. Set after construction via `set_cluster_manager`.
    pub(crate) cluster_manager: std::sync::OnceLock<Arc<crate::cluster::ClusterManager>>,
    /// One map for all deployed agents so `@N` survives internal A2A to another manager.
    pub(crate) shared_context_ref_store: SharedContextRefStore,
    pub(crate) external_tools_dirs: Vec<std::path::PathBuf>,
    pub(crate) sandbox_bind_roots: Vec<std::path::PathBuf>,
    /// Shared with the HTTP API so `/diagnose`'s `runtime_progress_lag_ms`
    /// reflects CPU pegs on the QuickJS thread (each booted agent registers a
    /// JS-event-loop probe), not just on the tokio runtime.
    pub(crate) runtime_progress: Arc<RuntimeProgressMeter>,
    /// When set, wrapped provenance writes notify the operator `/conversation-history` stream after commit.
    pub(crate) conversation_history_notify: Option<broadcast::Sender<ConversationHistoryUpdate>>,
    pub(crate) host_ingress_recorder: Arc<dyn baml_rt_core::HostIngressRecorder>,
}

impl AgentRunner {
    pub(crate) fn new(config: AgentRunnerConfig) -> baml_rt_core::Result<Self> {
        let routed_agents = std::sync::RwLock::new(HashMap::new());
        let internal_a2a_router = Arc::new(InternalA2aRouter::new());
        let provenance_store = config.provenance_config.store().clone();
        Ok(Self {
            agents: RwLock::new(HashMap::new()),
            provenance_config: config.provenance_config,
            deployment_state: config.deployment_state,
            access_policy: config.access_policy,
            routed_agents,
            internal_a2a_router,
            stream_idle_secs: config.stream_idle_secs,
            claude_workspaces_base: config.claude_workspaces_base,
            repository_url: config.repository_url,
            embedded_repository: config.embedded_repository,
            repository_http_client: reqwest::Client::new(),
            cluster_manager: std::sync::OnceLock::new(),
            shared_context_ref_store: SharedContextRefStore::new(),
            external_tools_dirs: config.external_tools_dirs,
            sandbox_bind_roots: config.sandbox_bind_roots,
            runtime_progress: config.runtime_progress,
            conversation_history_notify: config.conversation_history_notify,
            host_ingress_recorder: Arc::new(crate::services::HostIngressRecorderImpl::new(
                provenance_store,
            )),
        })
    }

    pub(crate) fn host_ingress_recorder(&self) -> Arc<dyn baml_rt_core::HostIngressRecorder> {
        self.host_ingress_recorder.clone()
    }

    pub(crate) fn external_tools_dirs(&self) -> &[std::path::PathBuf] {
        &self.external_tools_dirs
    }

    pub(crate) fn sandbox_bind_roots(&self) -> &[std::path::PathBuf] {
        &self.sandbox_bind_roots
    }

    pub(crate) fn conversation_history_notify_tx(
        &self,
    ) -> Option<broadcast::Sender<ConversationHistoryUpdate>> {
        self.conversation_history_notify.clone()
    }

    pub(crate) fn deployment_state(&self) -> &Arc<crate::deployment_state::DeploymentStateStore> {
        &self.deployment_state
    }

    pub(crate) fn provenance_config(&self) -> &ProvenanceConfig {
        &self.provenance_config
    }

    pub(crate) fn access_policy(&self) -> &ToolAccessPolicy {
        &self.access_policy
    }

    pub(crate) fn stream_idle_secs(&self) -> Option<u64> {
        self.stream_idle_secs
    }

    pub(crate) fn internal_a2a_router(&self) -> &Arc<InternalA2aRouter> {
        &self.internal_a2a_router
    }

    pub(crate) async fn requesting_task_still_in_flight(
        &self,
        requesting_agent_id: &str,
        context_id: &ContextId,
        task_id: &TaskId,
    ) -> bool {
        let agent = {
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents
                .values()
                .find(|agent| agent.agent_id().as_str() == requesting_agent_id)
                .cloned()
        };
        match agent {
            Some(agent) => agent.has_in_flight_turn(context_id, Some(task_id)).await,
            None => {
                tracing::warn!(
                    requesting_agent_id,
                    context_id = %context_id,
                    task_id = %task_id,
                    "system/callback could not find requesting agent while evaluating continuation deferral"
                );
                false
            }
        }
    }

    // ── Repository fetch ────────────────────────────────────────────────────

    pub(crate) async fn fetch_blob_from_repository(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<Vec<u8>> {
        if let Some(repo) = &self.embedded_repository {
            let hash = ContentHash::from_str(content_hash.as_str()).map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "embedded repository: invalid content hash: {e}"
                )))
            })?;
            match repo.get_blob(&hash).await {
                Ok(Some(bytes)) => {
                    tracing::trace!(
                        content_hash = content_hash.as_str(),
                        bytes_len = bytes.len(),
                        "repository blob read from embedded store"
                    );
                    return Ok(bytes);
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(BamlRtError::Io(std::io::Error::other(format!(
                        "embedded repository blob read failed for {}: {e}",
                        content_hash.as_str()
                    ))));
                }
            }
        }

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

    pub(crate) async fn fetch_repository_version(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<Option<u32>> {
        if let Some(repo) = &self.embedded_repository {
            let hash = ContentHash::from_str(content_hash.as_str()).map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "embedded repository: invalid content hash: {e}"
                )))
            })?;
            match repo.get_by_hash(&hash).await {
                Ok(Some(entry)) => {
                    let repository_version = entry.version_ref.version.as_u32();
                    tracing::trace!(
                        content_hash = content_hash.as_str(),
                        repository_version,
                        "repository entry read from embedded store"
                    );
                    return Ok(Some(repository_version));
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(BamlRtError::Io(std::io::Error::other(format!(
                        "embedded repository entry read failed for {}: {e}",
                        content_hash.as_str()
                    ))));
                }
            }
        }

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
        let Some(ver) = value.get("version_ref").and_then(|v| v.get("version")) else {
            return Err(BamlRtError::Io(std::io::Error::other(format!(
                "repository entry JSON missing version_ref for {}",
                content_hash.as_str()
            ))));
        };
        let Some(version) = parse_repository_entry_version(ver) else {
            return Err(BamlRtError::Io(std::io::Error::other(format!(
                "repository entry JSON has invalid version_ref.version for {}",
                content_hash.as_str()
            ))));
        };
        Ok(Some(version))
    }

    /// Verifies that blob bytes are a valid packaged agent and that the canonical
    /// [`SourceBundle::compute_hash`](baml_rt_repository::entry::SourceBundle::compute_hash)
    /// after [`with_manifest_version`](baml_rt_repository::entry::SourceBundle::with_manifest_version)
    /// with the repository entry's version equals `content_hash` (same scheme as
    /// [`MetadataStore::insert_entry`](baml_rt_repository::storage::MetadataStore::insert_entry)).
    pub(crate) fn verify_artifact_integrity(
        content_hash: &DeploymentContentHash,
        bytes: &[u8],
        repository_version: u32,
    ) -> Result<()> {
        let (_, source) =
            baml_rt_repository::package::source_bundle_from_tar_gz(bytes).map_err(|e| {
                BamlRtError::InvalidArgument(format!(
                    "Failed to extract source bundle from artifact: {e}"
                ))
            })?;
        let canonical = source
            .with_manifest_version(repository_version)
            .compute_hash();
        if canonical.as_str() != content_hash.as_str() {
            return Err(BamlRtError::InvalidArgument(format!(
                "Artifact content hash mismatch: deployment requests {expected}, but canonical hash from extracted bundle at repository version {repository_version} is {actual}",
                expected = content_hash.as_str(),
                actual = canonical.as_str(),
            )));
        }
        tracing::debug!(
            content_hash = content_hash.as_str(),
            repository_version,
            "Artifact blob verified (canonical hash matches repository content_hash)"
        );
        Ok(())
    }

    pub(crate) async fn boot_from_blob_bytes(
        &self,
        bytes: &[u8],
        content_hash: &DeploymentContentHash,
        repository_version: u32,
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
        let catalogue: Arc<dyn AgentLister> =
            if let Some(runner_arc) = self.internal_a2a_router().try_runner() {
                Arc::new(LiveAgentLister::new(Arc::downgrade(&runner_arc)))
            } else {
                Arc::new(SnapshotAgentLister {
                    entries: self.discovery_entries(),
                })
            };
        let (agent, _agent_id) = package
            .boot(AgentPackageBootArgs {
                shared_context_ref_store: self.shared_context_ref_store.clone(),
                provenance_config: self.provenance_config(),
                policy: self.access_policy(),
                agent_list_catalogue: catalogue,
                a2a_handler: scoped_router,
                stream_idle_secs: self.stream_idle_secs(),
                claude_workspaces_base: self.claude_workspaces_base.as_deref(),
                external_tools_dirs: self.external_tools_dirs(),
                sandbox_bind_roots: self.sandbox_bind_roots(),
                runtime_progress: self.runtime_progress.clone(),
                conversation_history_notify: self.conversation_history_notify_tx(),
            })
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
            provenance: DeploymentProvenance::Repository {
                content_hash: content_hash.clone(),
                version: repository_version,
            },
            lifecycle: Arc::new(AtomicU8::new(0)),
        };
        Ok((name, route_key, booted))
    }

    // ── Dispatch & discovery ─────────────────────────────────────────────────

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

    pub(crate) fn list_agents(&self) -> Vec<String> {
        self.agents
            .read()
            .expect("RwLock poison")
            .keys()
            .cloned()
            .collect()
    }

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
                    content_hash: booted.content_hash().map(|h| h.as_str().to_string()),
                    repository_version: booted.repository_version(),
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

    pub(crate) fn subscribe_task_update_receivers(
        &self,
    ) -> Vec<broadcast::Receiver<baml_rt_a2a::a2a_store::TaskUpdateEvent>> {
        let routed_agents = self.routed_agents.read().expect("RwLock poison");
        routed_agents
            .values()
            .map(|agent| agent.subscribe_task_updates())
            .collect()
    }

    pub(crate) async fn handle_a2a_by_key(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        // Check drain state and resolve route atomically so requests cannot
        // slip between a drain check and the route lookup.
        let routed_agent = {
            let agents = self.agents.read().expect("RwLock poison");
            if let Some(booted) = agents.get(key.agent_package.as_str())
                && booted.lifecycle_state() == AgentLifecycleState::Draining
            {
                let agent = key.agent_package.as_str();
                return Err(BamlRtError::AgentNotFound(format!(
                    "Agent {agent} is draining (undeploy in progress)",
                )));
            }
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents.get(key).cloned()
        };

        // Local fast path: agent is on this runner.
        if let Some(routed_agent) = routed_agent {
            let scope = scope_from_request(request.as_ref(), routed_agent.agent_id().clone());
            return context::with_scope(scope.as_scope().clone(), async move {
                routed_agent.handle_a2a_stream(request).await
            })
            .await;
        }

        // Cluster fallback: forward to the remote runner hosting this agent.
        if let Some(resolver) = self.internal_a2a_router.cluster_resolver() {
            match resolver.resolve(key).await {
                Ok(Some(placement)) => {
                    tracing::info!(
                        agent = %key.agent_package.as_str(),
                        endpoint = %placement.endpoint,
                        target_service_instance_id = %placement.service_instance_id,
                        "forwarding external A2A request to remote runner"
                    );
                    return self
                        .internal_a2a_router
                        .forward_to_runner(&placement, key, request)
                        .await;
                }
                Err(e) => {
                    let pkg = key.agent_package.as_str();
                    let inst = key.agent_instance_id.as_str();
                    return Err(BamlRtError::Io(std::io::Error::other(format!(
                        "cluster placement lookup failed for {pkg}/{inst}: {e}"
                    ))));
                }
                Ok(None) => {}
            }
        }

        let pkg = key.agent_package.as_str();
        let inst = key.agent_instance_id.as_str();
        Err(BamlRtError::AgentNotFound(format!(
            "Agent {pkg}/{inst} not found"
        )))
    }

    pub(crate) async fn handle_dispatch_by_key(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck> {
        let routed_agent = {
            let agents = self.agents.read().expect("RwLock poison");
            if let Some(booted) = agents.get(key.agent_package.as_str())
                && booted.lifecycle_state() == AgentLifecycleState::Draining
            {
                let agent = key.agent_package.as_str();
                return Err(BamlRtError::AgentNotFound(format!(
                    "Agent {agent} is draining (undeploy in progress)",
                )));
            }
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents.get(key).cloned().ok_or_else(|| {
                let pkg = key.agent_package.as_str();
                let inst = key.agent_instance_id.as_str();
                BamlRtError::AgentNotFound(format!("Agent {pkg}/{inst} not found"))
            })?
        };
        let link_event = callback_dispatch_context_link_event(&request, routed_agent.agent_id());
        let agent_package = key.agent_package.as_str().to_string();
        let agent_instance = key.agent_instance_id.as_str().to_string();
        let dispatch_snapshot = request.clone();
        let ack = routed_agent.handle_dispatch(request).await?;
        if ack.accepted {
            if let Err(err) = self
                .host_ingress_recorder
                .record_dispatch_accepted(
                    &dispatch_snapshot,
                    agent_package.as_str(),
                    agent_instance.as_str(),
                )
                .await
            {
                tracing::warn!(error = %err, "host dispatch accepted provenance write failed");
            }
            if let Some(event) = link_event {
                routed_agent
                    .provenance_writer()
                    .add_event_with_logging(event, "runner callback dispatch context link")
                    .await;
            }
        }
        Ok(ack)
    }

    // ── Stdio loop ───────────────────────────────────────────────────────────

    pub(crate) async fn run_a2a_loop<R, W>(&self, reader: R, mut writer: W) -> Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: AsyncWriteExt + Unpin,
    {
        use tokio::io::AsyncBufReadExt as _;
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
                    writer
                        .write_all(serialize_a2a_response(&response).as_bytes())
                        .await?;
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
                writer
                    .write_all(serialize_a2a_response(&response).as_bytes())
                    .await?;
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
                Ok(stream) => baml_rt_core::collect_a2a_stream_one_shot(stream)
                    .await
                    .into_iter()
                    .map(baml_rt_core::A2aStreamChunk::into_inner)
                    .collect(),
                Err(err) => vec![map_a2a_error(request_id, err)],
            };
            for response in responses {
                writer
                    .write_all(serialize_a2a_response(&response).as_bytes())
                    .await?;
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

        let agent_name = params
            .remove("agent")
            .and_then(|v| v.as_str().map(|s| s.to_string()));

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

// `?Send` matches core trait contract; deploy boot path is currently local-executor bound.
#[async_trait(?Send)]
impl DeploymentManager for AgentRunner {
    async fn deploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<baml_rt_core::DeployResult> {
        {
            let agents = self.agents.read().expect("RwLock poison");
            if agents
                .values()
                .any(|agent| agent.content_hash() == Some(content_hash))
            {
                return Ok(baml_rt_core::DeployResult {
                    already_deployed: true,
                });
            }
        }

        let repository_version = match self.fetch_repository_version(content_hash).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Repository has no entry for content hash {} — cannot verify artifact against canonical hash",
                    content_hash.as_str()
                )));
            }
            Err(err) => {
                return Err(BamlRtError::Io(std::io::Error::other(format!(
                    "Failed to load repository entry for {} (need version for canonical hash verification): {err}",
                    content_hash.as_str()
                ))));
            }
        };

        let bytes = self.fetch_blob_from_repository(content_hash).await?;
        AgentRunner::verify_artifact_integrity(content_hash, &bytes, repository_version)?;

        let package_name = manifest_package_name_from_tar_gz(&bytes).map_err(|e| {
            BamlRtError::InvalidArgument(format!(
                "Failed to read agent package manifest from artifact: {e}"
            ))
        })?;

        let mut old_hash_to_replace: Option<DeploymentContentHash> = None;
        {
            let agents = self.agents.read().expect("RwLock poison");
            if let Some(existing) = agents.get(&package_name) {
                match existing.content_hash() {
                    Some(h) if h == content_hash => {
                        return Ok(baml_rt_core::DeployResult {
                            already_deployed: true,
                        });
                    }
                    Some(h) => {
                        old_hash_to_replace = Some(h.clone());
                    }
                    None => {}
                }
            }
        }

        if let Some(ref old_hash) = old_hash_to_replace {
            tracing::info!(
                agent_package = %package_name,
                prior_content_hash = %old_hash.as_str(),
                new_content_hash = %content_hash.as_str(),
                "superseding deployment: draining prior revision before boot (up to 30s if traffic in flight)"
            );
            self.undeploy_by_hash(old_hash).await?;
        }

        let (name, route_key, booted) = self
            .boot_from_blob_bytes(&bytes, content_hash, repository_version)
            .await?;

        {
            let mut agents = self.agents.write().expect("RwLock poison");
            if agents
                .values()
                .any(|agent| agent.content_hash() == Some(content_hash))
            {
                return Ok(baml_rt_core::DeployResult {
                    already_deployed: true,
                });
            }
            if let Some(existing) = agents.get(&name)
                && existing.content_hash() != Some(content_hash)
            {
                return Err(BamlRtError::Conflict(format!(
                    "Agent '{name}' changed during deploy (concurrent deployment); retry"
                )));
            }
            let mut routed = self.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key.clone(), booted.agent.clone());
            agents.insert(name.clone(), booted);
        }

        let now = unix_timestamp_secs();
        if let Err(e) = self
            .deployment_state
            .save_deployment(&DeploymentRecord {
                content_hash: content_hash.clone(),
                agent_name: name.clone(),
                deployed_at: now.clone(),
                status: DeploymentStatus::Active,
                last_error: None,
                last_attempt_at: Some(now),
                failure_count: 0,
            })
            .await
        {
            // Roll back the in-memory maps so the agent isn't routable without
            // a persistent deployment record (it wouldn't survive a restart).
            tracing::error!(
                agent = %name,
                error = %e,
                "deployment state save failed; removing agent from maps to prevent ghost deployment"
            );
            let mut agents = self.agents.write().expect("RwLock poison");
            let mut routed = self.routed_agents.write().expect("RwLock poison");
            agents.remove(&name);
            routed.remove(&route_key);
            return Err(e);
        }
        tracing::info!(
            agent = %name,
            content_hash = %content_hash.as_str(),
            "deployment record persisted; POST /deploy completing"
        );

        // Record agent placement in cluster registry (if cluster mode is active).
        if let Some(cluster_mgr) = self.cluster_manager.get()
            && let Err(e) = cluster_mgr.record_placement(&route_key, content_hash).await
        {
            tracing::warn!(
                error = %e,
                agent = %route_key.agent_package.as_str(),
                "failed to record agent placement in cluster registry"
            );
        }

        Ok(baml_rt_core::DeployResult {
            already_deployed: false,
        })
    }

    async fn undeploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<UndeployResult> {
        // 1. Find the agent and mark as draining (stop accepting new requests).
        let (target_name, booted) = {
            let agents = self.agents.read().expect("RwLock poison");
            let entry = agents
                .iter()
                .find(|(_, agent)| agent.content_hash() == Some(content_hash));
            let Some((name, agent)) = entry else {
                return Ok(UndeployResult { removed: false });
            };
            agent.set_draining();
            (name.clone(), agent.clone())
        };

        tracing::info!(
            agent = %target_name,
            content_hash = %content_hash.as_str(),
            "undeploy: waiting for in-flight requests to finish (up to 30s)"
        );

        // 2. Wait for in-flight turns to complete (with timeout).
        let drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if !booted.agent.has_any_in_flight().await {
                break;
            }
            if tokio::time::Instant::now() >= drain_deadline {
                tracing::warn!(
                    agent = %target_name,
                    "drain timeout exceeded after 30s, proceeding with undeploy"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        tracing::info!(agent = %target_name, "undeploy: drain finished; removing from routing");

        // 3. Remove from both maps atomically to prevent dispatch to a
        //    destroyed agent between the two removals.
        {
            let mut agents = self.agents.write().expect("RwLock poison");
            let mut routed = self.routed_agents.write().expect("RwLock poison");
            agents.remove(&target_name);
            match AgentPackageName::parse(&booted.manifest.name) {
                Some(package_name) => {
                    let rk = AgentRouteKey::new(package_name, AgentInstanceId::default());
                    routed.remove(&rk);
                }
                None => {
                    tracing::error!(
                        name = %booted.manifest.name,
                        "agent package name invalid at undeploy; skipping routed_agents removal"
                    );
                }
            }
        }

        // 4. Remove placement from cluster registry.
        if let Some(cluster_mgr) = self.cluster_manager.get()
            && let Some(package_name) = AgentPackageName::parse(&target_name)
        {
            let rk = AgentRouteKey::new(package_name, AgentInstanceId::default());
            if let Err(e) = cluster_mgr.remove_placement(&rk).await {
                tracing::warn!(
                    agent = %target_name,
                    error = %e,
                    "failed to remove agent placement from cluster registry"
                );
            }
        }

        // 5. Delete deployment record.
        self.deployment_state
            .remove_deployment(content_hash)
            .await?;

        // 6. Emit AgentStopped provenance event last: if the process crashes
        //    before this point, the agent is already removed from routing and
        //    cluster — restart will re-deploy and emit a fresh AgentBooted
        //    without an orphaned AgentStopped in the graph.
        let agent_id = booted.agent.agent_id().clone();
        let stop_event = ProvEvent::agent_stopped(agent_id, "undeploy".to_string());
        let store = self.provenance_config.store();
        if let Err(e) = store.add_event(stop_event).await {
            tracing::warn!(
                agent = %target_name,
                error = %e,
                "failed to write AgentStopped provenance event"
            );
        }

        tracing::info!(agent = %target_name, "agent drained and undeployed");
        Ok(UndeployResult { removed: true })
    }

    async fn list_deployments(&self) -> Result<Vec<DeploymentRecord>> {
        self.deployment_state.list_deployments().await
    }
}
