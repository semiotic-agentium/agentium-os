// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! AgentRunner: host lifecycle — agent map, deploy/undeploy, dispatch, stdio loop.

use std::{
    collections::HashMap,
    pin::Pin,
    str::FromStr as _,
    sync::{Arc, RwLock, atomic::AtomicU8},
    task::{Context as TaskContext, Poll},
};

use async_trait::async_trait;
use baml_rt_a2a::{A2aRequestHandler, a2a};
use baml_rt_api::RuntimeProgressMeter;
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentLister, AgentPackageName, AgentRouteKey, ArtifactHash, BamlRtError,
    DeployedAgentLookup, DeploymentContentHash, DeploymentManager, DeploymentRecord,
    DeploymentStatus, DispatchTarget, ObservationUpdate, Result, UndeployResult,
    bus::BusStream,
    callback_scheduling_scopes_differ_from_dispatch, context, ensure_dispatch_task_scope,
    ids::{AgentId, ContextId, TaskId},
    scheduling_scope_from_dispatch_metadata,
};
use baml_rt_hash::AgentIdentityDigest;
use baml_rt_observability::spans;
use baml_rt_provenance::{
    ContextAgentFence, ContextAgentLease, ContextAgentLeaseRequest, LeaseAcquisition, ProvEvent,
    ProvenanceWriter, SurrealProvenanceStore,
};
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
    routing::{
        InternalA2aRouter, LiveAgentLister, ScopedInternalA2aRouter, scope_and_context_from_request,
    },
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

struct RepositoryDeploymentMetadata {
    repository_version: u32,
    artifact_hash: ArtifactHash,
    expected_agent_id: Option<AgentId>,
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
    pub(crate) repository_url: String,
    pub(crate) embedded_repository: Option<Arc<RepositoryService>>,
    pub(crate) runtime_progress: Arc<RuntimeProgressMeter>,
    pub(crate) observation_notify: Option<broadcast::Sender<ObservationUpdate>>,
    pub(crate) compaction_summarizer: Arc<dyn baml_rt_provenance::ConversationCompactionSummarizer>,
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
    pub(crate) repository_url: String,
    /// Same persistence as `GET /repository/*`; satisfies deploy/restore without depending on HTTP.
    pub(crate) embedded_repository: Option<Arc<RepositoryService>>,
    pub(crate) repository_http_client: reqwest::Client,
    /// Cluster manager for recording agent placements. Set after construction via `set_cluster_manager`.
    pub(crate) cluster_manager: std::sync::OnceLock<Arc<crate::cluster::ClusterManager>>,
    /// One map for all deployed agents so `@N` survives internal A2A to another manager.
    pub(crate) shared_context_ref_store: SharedContextRefStore,
    /// Shared with the HTTP API so `/diagnose`'s `runtime_progress_lag_ms`
    /// reflects CPU pegs on the QuickJS thread (each booted agent registers a
    /// JS-event-loop probe), not just on the tokio runtime.
    pub(crate) runtime_progress: Arc<RuntimeProgressMeter>,
    /// When set, wrapped provenance writes notify the operator `/conversation-history` stream after commit.
    pub(crate) observation_notify: Option<broadcast::Sender<ObservationUpdate>>,
    pub(crate) host_ingress_recorder: Arc<dyn baml_rt_core::HostIngressRecorder>,
    pub(crate) compaction_summarizer: Arc<dyn baml_rt_provenance::ConversationCompactionSummarizer>,
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
            repository_url: config.repository_url,
            embedded_repository: config.embedded_repository,
            repository_http_client: reqwest::Client::new(),
            cluster_manager: std::sync::OnceLock::new(),
            shared_context_ref_store: SharedContextRefStore::new(),
            runtime_progress: config.runtime_progress,
            observation_notify: config.observation_notify,
            host_ingress_recorder: Arc::new(crate::services::HostIngressRecorderImpl::new(
                provenance_store,
            )),
            compaction_summarizer: config.compaction_summarizer,
        })
    }

    pub(crate) fn host_ingress_recorder(&self) -> Arc<dyn baml_rt_core::HostIngressRecorder> {
        self.host_ingress_recorder.clone()
    }

    pub(crate) fn observation_notify_tx(&self) -> Option<broadcast::Sender<ObservationUpdate>> {
        self.observation_notify.clone()
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

    /// Stable operational identity for lease rows and owner preference.
    ///
    /// Cluster mode uses the cluster runner id and routable endpoint. Standalone
    /// mode still gets a stable per-process identity so lease fencing works.
    pub(crate) fn lease_runner_identity(&self) -> (String, Option<String>) {
        match self.cluster_manager.get() {
            Some(manager) => (
                manager.runner_id().to_string(),
                Some(manager.endpoint().to_string()),
            ),
            None => (
                baml_rt_observability::service_instance_id().to_string(),
                None,
            ),
        }
    }

    pub(crate) fn internal_a2a_router(&self) -> &Arc<InternalA2aRouter> {
        &self.internal_a2a_router
    }

    /// Admit one turn against the context route binding before dispatch.
    ///
    /// The ordinary deployment resolves the route to a pair. If the context
    /// already pinned a different pair (a deploy moved the route between two
    /// turns), the turn is refused instead of executing under another revision.
    ///
    /// Ephemeral boots carry no artifact hash, so they have no exact package to
    /// pin and skip the admission check (development and test agents only).
    pub(crate) async fn admit_context_route_binding(
        &self,
        key: &AgentRouteKey,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<()> {
        let artifact_hash = {
            let agents = self.agents.read().expect("RwLock poison");
            agents
                .get(key.agent_package.as_str())
                .and_then(|booted| booted.artifact_hash().cloned())
        };
        let Some(artifact_hash) = artifact_hash else {
            return Ok(());
        };
        self.admit_context_route_binding_resolved(key, context_id, agent_id, &artifact_hash)
            .await
    }

    /// Admit a turn against a binding using an already-resolved remote pair.
    ///
    /// Cluster placement carries the target's stable `AgentId` and exact
    /// artifact, so a remote target is admitted before forwarding instead of
    /// after the receiving runner has already been chosen.
    pub(crate) async fn admit_context_route_binding_resolved(
        &self,
        key: &AgentRouteKey,
        context_id: &ContextId,
        agent_id: &AgentId,
        artifact_hash: &ArtifactHash,
    ) -> Result<()> {
        let request = baml_rt_provenance::ContextRouteBindingRequest::new(
            context_id.clone(),
            key.agent_package.as_str(),
            agent_id.clone(),
            artifact_hash.clone(),
        );
        match self
            .provenance_config
            .store()
            .resolve_context_route_binding(request)
            .await
        {
            Ok(_) => Ok(()),
            Err(baml_rt_provenance::ProvenanceError::ContextRouteBindingConflict(conflict)) => {
                Err(BamlRtError::ContextBindingConflict(Box::new(
                    baml_rt_core::ContextBindingConflict {
                        context_id: conflict.context_id,
                        package_name: conflict.package_name,
                        pinned_agent_id: conflict.pinned_agent_id,
                        pinned_artifact_hash: conflict.pinned_artifact_hash,
                        resolved_agent_id: conflict.resolved_agent_id,
                        resolved_artifact_hash: conflict.resolved_artifact_hash,
                    },
                )))
            }
            Err(error) => Err(BamlRtError::Io(std::io::Error::other(format!(
                "context route binding resolution failed for {} in {context_id}: {error}",
                key.agent_package.as_str()
            )))),
        }
    }

    /// Claim or confirm this boot's lease for the target pair.
    ///
    /// `NotApplicable` means the request shape or local boot metadata does not
    /// carry a context-agent pair (for example an unparseable test request).
    pub(crate) async fn claim_local_pair_lease(
        &self,
        key: &AgentRouteKey,
        routed_agent: &baml_rt_a2a::A2aAgent,
        context_id: Option<baml_rt_core::ids::ContextId>,
    ) -> Result<PairLeaseClaim> {
        let Some(context_id) = context_id else {
            return Ok(PairLeaseClaim::NotApplicable);
        };
        let booted = {
            let agents = self.agents.read().expect("RwLock poison");
            agents.get(key.agent_package.as_str()).cloned()
        };
        let Some(booted) = booted else {
            return Ok(PairLeaseClaim::NotApplicable);
        };

        let (runner_id, endpoint) = self.lease_runner_identity();
        let mut request = ContextAgentLeaseRequest::new(
            context_id.clone(),
            routed_agent.agent_id().clone(),
            booted.boot_event_id.clone(),
            runner_id,
            baml_rt_core::now_unix_ms(baml_rt_core::clock_events::PROVENANCE_EVENT),
        );
        if let Some(endpoint) = endpoint {
            request = request.with_endpoint(endpoint);
        }
        if let Some(artifact_hash) = booted.artifact_hash() {
            request = request.with_artifact_hash(artifact_hash.clone());
        }

        match self
            .provenance_config
            .store()
            .acquire_context_agent_lease(request)
            .await
        {
            Ok(LeaseAcquisition::Acquired { lease, .. }) => Ok(PairLeaseClaim::Owned {
                context_id,
                fence: lease.fence(),
            }),
            Ok(LeaseAcquisition::HeldByOther { lease }) => Ok(PairLeaseClaim::HeldElsewhere(lease)),
            Err(baml_rt_provenance::ProvenanceError::LeaseArtifactMismatch {
                held_artifact_hash,
                requested_artifact_hash,
                ..
            }) => Err(BamlRtError::Conflict(format!(
                "context {context_id} agent {} is owned by artifact {held_artifact_hash};                  this boot runs {requested_artifact_hash}",
                routed_agent.agent_id().as_str()
            ))),
            Err(error) => Err(BamlRtError::Io(std::io::Error::other(format!(
                "context-agent lease acquisition failed for {} in {context_id}: {error}",
                routed_agent.agent_id().as_str()
            )))),
        }
    }

    /// Wrap a local A2A stream so the pair lease is released once no session
    /// for the context remains open.
    pub(crate) fn release_pair_lease_when_idle(
        &self,
        stream: BusStream<A2aStreamChunk>,
        routed_agent: baml_rt_a2a::A2aAgent,
        context_id: baml_rt_core::ids::ContextId,
        fence: ContextAgentFence,
    ) -> BusStream<A2aStreamChunk> {
        Box::pin(LeaseReleaseStream {
            inner: stream,
            release: Some(ReleaseOnIdle {
                store: self.provenance_config.store().clone(),
                agent: routed_agent,
                context_id,
                fence,
            }),
        })
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
        artifact_hash: &ArtifactHash,
    ) -> Result<Vec<u8>> {
        if let Some(repo) = &self.embedded_repository {
            match repo.get_blob(artifact_hash).await {
                Ok(Some(bytes)) => {
                    tracing::trace!(
                        artifact_hash = artifact_hash.as_str(),
                        bytes_len = bytes.len(),
                        "repository blob read from embedded store"
                    );
                    return Ok(bytes);
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(BamlRtError::Io(std::io::Error::other(format!(
                        "embedded repository blob read failed for {}: {e}",
                        artifact_hash.as_str()
                    ))));
                }
            }
        }

        let url = format!(
            "{}/blobs/{}",
            self.repository_url.trim_end_matches('/'),
            artifact_hash.as_str()
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
                "Repository blob not found for artifact hash {}",
                artifact_hash.as_str()
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
                artifact_hash.as_str()
            )))
        })
    }

    async fn fetch_repository_version(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<Option<RepositoryDeploymentMetadata>> {
        if let Some(repo) = &self.embedded_repository {
            let hash = ContentHash::from_str(content_hash.as_str()).map_err(|e| {
                BamlRtError::Io(std::io::Error::other(format!(
                    "embedded repository: invalid content hash: {e}"
                )))
            })?;
            match repo.get_by_hash(&hash).await {
                Ok(Some(entry)) => {
                    let repository_version = entry.version_ref.version.as_u32();
                    let artifact_hash = entry.artifact_hash.ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "repository entry {} is source-only (no artifact hash) and cannot be deployed; republish through the build path",
                            content_hash.as_str()
                        ))
                    })?;
                    tracing::trace!(
                        content_hash = content_hash.as_str(),
                        artifact_hash = artifact_hash.as_str(),
                        repository_version,
                        "repository entry read from embedded store"
                    );
                    return Ok(Some(RepositoryDeploymentMetadata {
                        repository_version,
                        artifact_hash,
                        expected_agent_id: entry
                            .identity_digest
                            .as_ref()
                            .map(AgentId::from_identity_digest),
                    }));
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
        let artifact_hash = value
            .get("artifact_hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(format!(
                    "repository entry JSON missing artifact_hash for {}",
                    content_hash.as_str()
                ))
            })?
            .parse::<ArtifactHash>()
            .map_err(|e| {
                BamlRtError::InvalidArgument(format!(
                    "repository entry JSON has invalid artifact_hash for {}: {e}",
                    content_hash.as_str()
                ))
            })?;
        let expected_agent_id = value
            .get("identity_digest")
            .and_then(serde_json::Value::as_str)
            .map(str::parse::<AgentIdentityDigest>)
            .transpose()
            .map_err(|e| {
                BamlRtError::InvalidArgument(format!(
                    "repository entry JSON has invalid identity_digest for {}: {e}",
                    content_hash.as_str()
                ))
            })?
            .as_ref()
            .map(AgentId::from_identity_digest);
        Ok(Some(RepositoryDeploymentMetadata {
            repository_version: version,
            artifact_hash,
            expected_agent_id,
        }))
    }

    /// Verifies that blob bytes are a valid packaged agent and that the canonical
    /// [`SourceBundle::compute_hash`](baml_rt_repository::entry::SourceBundle::compute_hash)
    /// after [`with_manifest_version`](baml_rt_repository::entry::SourceBundle::with_manifest_version)
    /// with the repository entry's version equals `content_hash` (same scheme as
    /// [`MetadataStore::insert_entry`](baml_rt_repository::storage::MetadataStore::insert_entry)).
    pub(crate) fn verify_artifact_integrity(
        content_hash: &DeploymentContentHash,
        artifact_hash: &ArtifactHash,
        bytes: &[u8],
        repository_version: u32,
    ) -> Result<()> {
        let computed_artifact_hash =
            baml_rt_repository::artifact_hash_from_tar_gz(bytes).map_err(|e| {
                BamlRtError::InvalidArgument(format!(
                    "Artifact integrity failure: invalid agent artifact: {e}"
                ))
            })?;
        if &computed_artifact_hash != artifact_hash {
            return Err(BamlRtError::InvalidArgument(format!(
                "Artifact integrity failure: artifact identity expected {artifact_hash}, computed {computed_artifact_hash}"
            )));
        }
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
            artifact_hash = artifact_hash.as_str(),
            repository_version,
            "Artifact blob verified against artifact and deployment content hashes"
        );
        Ok(())
    }

    pub(crate) async fn boot_from_blob_bytes(
        &self,
        bytes: &[u8],
        content_hash: &DeploymentContentHash,
        artifact_hash: &ArtifactHash,
        repository_version: u32,
        expected_agent_id: Option<&AgentId>,
    ) -> Result<(String, AgentRouteKey, BootedAgent)> {
        let computed_artifact_hash =
            baml_rt_repository::artifact_hash_from_tar_gz(bytes).map_err(|e| {
                BamlRtError::InvalidArgument(format!(
                    "Artifact integrity failure: invalid agent artifact: {e}"
                ))
            })?;
        if &computed_artifact_hash != artifact_hash {
            return Err(BamlRtError::InvalidArgument(format!(
                "Artifact integrity failure: artifact identity expected {artifact_hash}, computed {computed_artifact_hash}"
            )));
        }
        let mut package_file = tempfile::Builder::new()
            .prefix("baml-deploy-")
            .suffix(".tar.gz")
            .tempfile()
            .map_err(BamlRtError::Io)?;
        use std::io::Write as _;
        package_file.write_all(bytes).map_err(BamlRtError::Io)?;
        package_file.flush().map_err(BamlRtError::Io)?;

        let package = AgentPackage::load_from_file(package_file.path()).await?;
        let identity_digest = package.identity_digest()?;
        let agent_id = AgentId::from_identity_digest(&identity_digest);
        if let Some(expected_agent_id) = expected_agent_id
            && &agent_id != expected_agent_id
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "Agent identity mismatch: deployment expects {expected_agent_id}, package derives {agent_id}"
            )));
        }
        let name = package.name().to_string();
        let package_name = AgentPackageName::parse(&name).ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "Agent package name '{name}' is invalid; allowed characters: [A-Za-z0-9_-]"
            ))
        })?;
        let route_key = AgentRouteKey::new(package_name);
        let scoped_router: Arc<dyn A2aRequestHandler> = Arc::new(ScopedInternalA2aRouter::new(
            route_key.clone(),
            agent_id.clone(),
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
        let (agent, booted_agent_id, boot_event_id, tool_host) = package
            .boot(AgentPackageBootArgs {
                agent_id: agent_id.clone(),
                artifact_hash: artifact_hash.clone(),
                identity_digest,
                deployment_content_hash: content_hash.clone(),
                repository_version,
                shared_context_ref_store: self.shared_context_ref_store.clone(),
                provenance_config: self.provenance_config(),
                policy: self.access_policy(),
                agent_list_catalogue: catalogue,
                a2a_handler: scoped_router,
                stream_idle_secs: self.stream_idle_secs(),
                runtime_progress: self.runtime_progress.clone(),
                observation_notify: self.observation_notify_tx(),
                compaction_summarizer: Arc::clone(&self.compaction_summarizer),
            })
            .await?;
        if booted_agent_id != agent_id {
            return Err(BamlRtError::InvalidArgument(format!(
                "agent boot identity mismatch: expected {agent_id}, booted {booted_agent_id}"
            )));
        }
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
                artifact_hash: artifact_hash.clone(),
                version: repository_version,
            },
            lifecycle: Arc::new(AtomicU8::new(0)),
            boot_event_id,
            tool_host,
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
        self.handle_a2a_by_key_with_precondition(key, request, None, None)
            .await
    }

    /// Resolve the route and verify optional expected identity against the exact
    /// booted agent selected for dispatch, before any task or provenance write.
    ///
    /// The receiver checks both the stable `AgentId` and the exact artifact hash:
    /// two artifacts can share one `AgentId`, so an ID-only check would accept a
    /// runner carrying different bytes.
    pub(crate) async fn handle_a2a_by_key_with_precondition(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
        expected_agent_id: Option<&AgentId>,
        expected_artifact_hash: Option<&ArtifactHash>,
    ) -> Result<BusStream<A2aStreamChunk>> {
        // Check drain state and resolve route atomically so requests cannot
        // slip between a drain check and the route lookup.
        let routed_agent = {
            let agents = self.agents.read().expect("RwLock poison");
            if let Some(booted) = agents.get(key.agent_package.as_str()) {
                if booted.lifecycle_state() == AgentLifecycleState::Draining {
                    let agent = key.agent_package.as_str();
                    return Err(BamlRtError::AgentDraining(format!(
                        "Agent {agent} is draining (undeploy in progress)",
                    )));
                }
                if let Some(expected) = expected_agent_id {
                    let actual = booted.agent.agent_id();
                    if actual != expected {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "agent precondition failed for {}: expected {expected}, serving {actual}",
                            key.agent_package.as_str()
                        )));
                    }
                }
                if let Some(expected) = expected_artifact_hash {
                    let actual = booted.artifact_hash().ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "agent {} has no artifact hash for precondition verification",
                            key.agent_package.as_str()
                        ))
                    })?;
                    if actual != expected {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "artifact precondition failed for {}: expected {expected}, serving {actual}",
                            key.agent_package.as_str()
                        )));
                    }
                }
            }
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents.get(key).cloned()
        };

        // Local fast path: agent is on this runner. Ownership is checked before
        // execution so a stale or secondary boot never writes the pair.
        if let Some(routed_agent) = routed_agent {
            let (scope, context_id) =
                scope_and_context_from_request(request.as_ref(), routed_agent.agent_id().clone());
            if let Some(context_id) = context_id.as_ref() {
                self.admit_context_route_binding(key, context_id, routed_agent.agent_id())
                    .await?;
            }
            match self
                .claim_local_pair_lease(key, &routed_agent, context_id)
                .await?
            {
                PairLeaseClaim::Owned { context_id, fence } => {
                    let agent_for_stream = routed_agent.clone();
                    let stream = context::with_scope(scope.as_scope().clone(), async move {
                        agent_for_stream.handle_a2a_stream(request).await
                    })
                    .await?;
                    return Ok(self.release_pair_lease_when_idle(
                        stream,
                        routed_agent,
                        context_id,
                        fence,
                    ));
                }
                PairLeaseClaim::HeldElsewhere(owner) => {
                    return self
                        .internal_a2a_router
                        .forward_to_lease_owner(key, &owner, request)
                        .await;
                }
                PairLeaseClaim::NotApplicable => {
                    return context::with_scope(scope.as_scope().clone(), async move {
                        routed_agent.handle_a2a_stream(request).await
                    })
                    .await;
                }
            }
        }

        // Cluster fallback: forward to the remote runner hosting this agent.
        if let Some(resolver) = self.internal_a2a_router.cluster_resolver() {
            match resolver.resolve(key).await {
                Ok(Some(placement)) => {
                    let (_, context_id) = scope_and_context_from_request(
                        request.as_ref(),
                        placement.agent_id.clone(),
                    );
                    if let Some(context_id) = context_id.as_ref() {
                        self.admit_context_route_binding_resolved(
                            key,
                            context_id,
                            &placement.agent_id,
                            &placement.artifact_hash,
                        )
                        .await?;
                    }
                    tracing::info!(
                        agent = %key.agent_package.as_str(),
                        endpoint = %placement.endpoint,
                        target_service_instance_id = %placement.service_instance_id,
                        agent_id = %placement.agent_id.as_str(),
                        artifact_hash = %placement.artifact_hash.as_str(),
                        "forwarding external A2A request to remote runner"
                    );
                    if let Some(context_id) = context_id.as_ref()
                        && let Some(owner) = self
                            .provenance_config
                            .store()
                            .read_context_agent_lease(context_id, &placement.agent_id)
                            .await
                            .map_err(|error| {
                                BamlRtError::Io(std::io::Error::other(format!(
                                    "context-agent lease read failed for {} in {context_id}: {error}",
                                    key.agent_package.as_str()
                                )))
                            })?
                    {
                        return self
                            .internal_a2a_router
                            .forward_to_lease_owner(key, &owner, request)
                            .await;
                    }
                    return self
                        .internal_a2a_router
                        .forward_to_runner(&placement, key, request)
                        .await;
                }
                Err(e) => {
                    let pkg = key.agent_package.as_str();
                    return Err(BamlRtError::Io(std::io::Error::other(format!(
                        "cluster placement lookup failed for {pkg}: {e}"
                    ))));
                }
                Ok(None) => {}
            }
        }

        let pkg = key.agent_package.as_str();
        Err(BamlRtError::AgentNotFound(format!("Agent {pkg} not found")))
    }

    pub(crate) async fn handle_dispatch_by_key(
        &self,
        key: &AgentRouteKey,
        mut request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck> {
        let routed_agent = {
            let agents = self.agents.read().expect("RwLock poison");
            if let Some(booted) = agents.get(key.agent_package.as_str())
                && booted.lifecycle_state() == AgentLifecycleState::Draining
            {
                let agent = key.agent_package.as_str();
                return Err(BamlRtError::AgentDraining(format!(
                    "Agent {agent} is draining (undeploy in progress)",
                )));
            }
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents.get(key).cloned().ok_or_else(|| {
                let pkg = key.agent_package.as_str();
                BamlRtError::AgentNotFound(format!("Agent {pkg} not found"))
            })?
        };
        ensure_dispatch_task_scope(&mut request, key);
        let link_event = callback_dispatch_context_link_event(&request, routed_agent.agent_id());
        let dispatch_target = DispatchTarget::new(key.clone(), routed_agent.agent_id().clone());
        let dispatch_snapshot = request.clone();
        let ack = routed_agent.handle_dispatch(request).await?;
        if ack.accepted {
            if let Err(err) = self
                .host_ingress_recorder
                .record_dispatch_accepted(&dispatch_snapshot, dispatch_target.clone())
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
        } else if let Err(err) = self
            .host_ingress_recorder
            .record_dispatch_rejected(
                &dispatch_snapshot,
                dispatch_target,
                ack.detail.as_deref().unwrap_or("rejected"),
                false,
            )
            .await
        {
            tracing::warn!(error = %err, "host dispatch rejected provenance write failed");
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
impl AgentRunner {
    /// Deploy a repository entry and pin the exact artifact hash the caller expects.
    ///
    /// `expected_artifact_hash` is used by startup restore so a repository state change cannot
    /// silently boot different bytes than the deployment record names.
    pub(crate) async fn deploy_by_hash_expected(
        &self,
        content_hash: &DeploymentContentHash,
        expected_artifact_hash: Option<&ArtifactHash>,
    ) -> Result<baml_rt_core::DeployResult> {
        {
            let agents = self.agents.read().expect("RwLock poison");
            if let Some(agent) = agents
                .values()
                .find(|agent| agent.content_hash() == Some(content_hash))
            {
                if let Some(expected) = expected_artifact_hash
                    && agent.artifact_hash() != Some(expected)
                {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "Artifact integrity failure: content hash {} is already deployed with artifact {:?}, expected {expected}",
                        content_hash.as_str(),
                        agent.artifact_hash().map(ArtifactHash::as_str)
                    )));
                }
                return Ok(baml_rt_core::DeployResult {
                    already_deployed: true,
                });
            }
        }

        let deployment = match self.fetch_repository_version(content_hash).await {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Repository has no entry for content hash {} — cannot resolve its artifact hash",
                    content_hash.as_str()
                )));
            }
            Err(err) => {
                return Err(BamlRtError::Io(std::io::Error::other(format!(
                    "Failed to load repository entry for {} (need version and artifact hash): {err}",
                    content_hash.as_str()
                ))));
            }
        };
        let RepositoryDeploymentMetadata {
            repository_version,
            artifact_hash,
            expected_agent_id,
        } = deployment;
        if let Some(expected) = expected_artifact_hash
            && &artifact_hash != expected
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "Artifact integrity failure: repository content hash {} resolves to artifact {artifact_hash}, expected {expected}",
                content_hash.as_str()
            )));
        }

        let bytes = self.fetch_blob_from_repository(&artifact_hash).await?;
        AgentRunner::verify_artifact_integrity(
            content_hash,
            &artifact_hash,
            &bytes,
            repository_version,
        )?;

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
            self.undeploy_by_hash(old_hash, None).await?;
        }

        let (name, route_key, booted) = self
            .boot_from_blob_bytes(
                &bytes,
                content_hash,
                &artifact_hash,
                repository_version,
                expected_agent_id.as_ref(),
            )
            .await?;
        let booted_artifact_hash = booted.artifact_hash().cloned().ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "repository deployment booted without an artifact hash".to_string(),
            )
        })?;
        if booted_artifact_hash != artifact_hash {
            return Err(BamlRtError::InvalidArgument(format!(
                "Artifact integrity failure: booted package recorded {booted_artifact_hash}, expected {artifact_hash}"
            )));
        }
        let booted_agent_id = booted.agent.agent_id().clone();

        {
            let mut agents = self.agents.write().expect("RwLock poison");
            if let Some(agent) = agents
                .values()
                .find(|agent| agent.content_hash() == Some(content_hash))
            {
                if let Some(expected) = expected_artifact_hash
                    && agent.artifact_hash() != Some(expected)
                {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "Artifact integrity failure: content hash {} is already deployed with artifact {:?}, expected {expected}",
                        content_hash.as_str(),
                        agent.artifact_hash().map(ArtifactHash::as_str)
                    )));
                }
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
                artifact_hash: booted_artifact_hash.clone(),
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
            && let Err(e) = cluster_mgr
                .record_placement_with_artifact(
                    &route_key,
                    content_hash,
                    Some(&booted_artifact_hash),
                    Some(&booted_agent_id),
                )
                .await
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
}

#[async_trait(?Send)]
impl DeploymentManager for AgentRunner {
    async fn deploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
    ) -> Result<baml_rt_core::DeployResult> {
        self.deploy_by_hash_expected(content_hash, None).await
    }

    async fn undeploy_by_hash(
        &self,
        content_hash: &DeploymentContentHash,
        override_rationale: Option<&str>,
    ) -> Result<UndeployResult> {
        // 1. Find the agent. Do not mark it draining until the deploy guard has
        //    admitted the removal: a refused undeploy must leave the agent
        //    serving.
        let (target_name, booted) = {
            let agents = self.agents.read().expect("RwLock poison");
            let entry = agents
                .iter()
                .find(|(_, agent)| agent.content_hash() == Some(content_hash));
            let Some((name, agent)) = entry else {
                return Ok(UndeployResult { removed: false });
            };
            (name.clone(), agent.clone())
        };

        // 1a. Refuse to remove a revision recently active contexts still pin,
        //     unless the operator recorded an explicit override.
        let package = AgentPackageName::parse(&target_name);
        let artifact_hash = booted.artifact_hash().cloned();
        if let (Some(package), Some(artifact_hash)) = (package, artifact_hash) {
            let store = self.provenance_config.store();
            let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::PROVENANCE_EVENT);
            let active_since_ms =
                now_ms.saturating_sub(baml_rt_provenance::DEFAULT_CONTEXT_ACTIVITY_RETENTION_MS);
            let pinned = store
                .list_pinned_context_activity(package.as_str(), &artifact_hash, active_since_ms)
                .await
                .map_err(|error| {
                    BamlRtError::Io(std::io::Error::other(format!(
                        "undeploy could not read pinned contexts for {}: {error}",
                        package.as_str()
                    )))
                })?;
            if !pinned.is_empty() {
                let contexts: Vec<ContextId> =
                    pinned.iter().map(|row| row.context_id.clone()).collect();
                let Some(rationale) = override_rationale.filter(|value| !value.trim().is_empty())
                else {
                    return Err(BamlRtError::ContextsPinnedToRevision(Box::new(
                        baml_rt_core::ContextsPinnedToRevision {
                            package_name: package.as_str().to_string(),
                            artifact_hash: artifact_hash.as_str().to_string(),
                            contexts: contexts
                                .iter()
                                .map(|context| context.as_str().to_string())
                                .collect(),
                        },
                    )));
                };
                store
                    .record_undeploy_override(
                        package.as_str(),
                        booted.agent.agent_id(),
                        &artifact_hash,
                        &contexts,
                        rationale,
                        None,
                    )
                    .await
                    .map_err(|error| {
                        BamlRtError::Io(std::io::Error::other(format!(
                            "undeploy override audit write failed for {}: {error}",
                            package.as_str()
                        )))
                    })?;
            }
        }
        booted.set_draining();

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

        if let Some(tool_host) = &booted.tool_host
            && let Err(error) = tool_host
                .release_agent(&booted.agent.agent_id().to_string())
                .await
        {
            tracing::warn!(
                agent = %target_name,
                error = %error,
                "failed to release pooled tool VMs during undeploy"
            );
        }

        if let Err(error) = self
            .provenance_config
            .store()
            .release_context_agent_leases_for_boot(
                &booted.boot_event_id,
                baml_rt_core::now_unix_ms(baml_rt_core::clock_events::PROVENANCE_EVENT),
            )
            .await
        {
            tracing::warn!(
                agent = %target_name,
                error = %error,
                "failed to release context-agent leases during undeploy"
            );
        }

        // 3. Remove from both maps atomically to prevent dispatch to a
        //    destroyed agent between the two removals.
        {
            let mut agents = self.agents.write().expect("RwLock poison");
            let mut routed = self.routed_agents.write().expect("RwLock poison");
            agents.remove(&target_name);
            match AgentPackageName::parse(&booted.manifest.name) {
                Some(package_name) => {
                    let rk = AgentRouteKey::new(package_name);
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
            let rk = AgentRouteKey::new(package_name);
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
        let stop_event = ProvEvent::agent_stopped(
            agent_id,
            booted.boot_event_id.clone(),
            "undeploy".to_string(),
        );
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

/// Result of claiming execution ownership for a local target.
pub(crate) enum PairLeaseClaim {
    Owned {
        context_id: baml_rt_core::ids::ContextId,
        fence: ContextAgentFence,
    },
    HeldElsewhere(ContextAgentLease),
    NotApplicable,
}

struct LeaseReleaseStream {
    inner: BusStream<A2aStreamChunk>,
    release: Option<ReleaseOnIdle>,
}

impl LeaseReleaseStream {
    fn release_when_idle(&mut self) {
        if let Some(release) = self.release.take() {
            tokio::spawn(async move {
                release.release_if_idle().await;
            });
        }
    }
}

impl futures_util::Stream for LeaseReleaseStream {
    type Item = A2aStreamChunk;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(cx);
        if matches!(polled, Poll::Ready(None)) {
            self.release_when_idle();
        }
        polled
    }
}

impl Drop for LeaseReleaseStream {
    fn drop(&mut self) {
        self.release_when_idle();
    }
}

struct ReleaseOnIdle {
    store: Arc<SurrealProvenanceStore>,
    agent: baml_rt_a2a::A2aAgent,
    context_id: baml_rt_core::ids::ContextId,
    fence: ContextAgentFence,
}

impl ReleaseOnIdle {
    async fn release_if_idle(self) {
        if self
            .agent
            .has_open_session_for_context(&self.context_id)
            .await
        {
            return;
        }
        let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::PROVENANCE_EVENT);
        if let Err(error) = self
            .store
            .release_context_agent_lease(&self.fence, now_ms)
            .await
        {
            tracing::warn!(
                context_id = %self.context_id,
                agent_id = %self.fence.agent_id,
                error = %error,
                "failed to release context-agent lease after stream ended"
            );
        }
    }
}

impl DeployedAgentLookup for AgentRunner {
    fn agent_id_for_route(&self, route: &AgentRouteKey) -> Option<AgentId> {
        let routed_agents = self.routed_agents.read().expect("RwLock poison");
        routed_agents
            .get(route)
            .map(|agent| agent.agent_id().clone())
    }
}
