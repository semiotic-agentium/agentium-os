//! AgentRunner: host lifecycle — agent map, deploy/undeploy, dispatch, stdio loop.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use baml_rt_a2a::{A2aRequestHandler, a2a};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentInstanceId, AgentLister,
    AgentPackageName, AgentRouteKey, BamlRtError, DeploymentContentHash, DeploymentManager,
    DeploymentRecord, DeploymentStatus, Result, UndeployResult, bus::BusStream, context,
    ids::{ContextId, TaskId},
};
use baml_rt_observability::spans;
use baml_rt_provenance::ToolIndexConfig;
use baml_rt_tools::ToolAccessPolicy;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::{
    agent_package::{AgentPackage, BootedAgent, SnapshotAgentLister},
    config::ProvenanceConfig,
    routing::{InternalA2aRouter, ScopedInternalA2aRouter, scope_from_request},
    stdio::{
        is_a2a_method, map_a2a_error, select_implicit_stdio_agent, serialize_a2a_response,
        split_agent_method, strip_stream_suffix, unix_timestamp_secs, wrap_plaintext_message,
    },
};

fn parse_repository_entry_version(value: &serde_json::Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return u32::try_from(n).ok();
    }
    if let Some(s) = value.as_str() {
        return s.strip_prefix('v').unwrap_or(s).parse::<u32>().ok();
    }
    None
}

/// Agent runner host: manages agents and composes the tool catalogue at startup.
pub(crate) struct AgentRunner {
    pub(crate) agents: RwLock<HashMap<String, BootedAgent>>,
    pub(crate) provenance_config: ProvenanceConfig,
    pub(crate) deployment_state: Arc<crate::deployment_state::DeploymentStateStore>,
    pub(crate) tool_index: Option<ToolIndexConfig>,
    pub(crate) access_policy: ToolAccessPolicy,
    pub(crate) routed_agents: std::sync::RwLock<HashMap<AgentRouteKey, baml_rt_a2a::A2aAgent>>,
    pub(crate) internal_a2a_router: Arc<InternalA2aRouter>,
    pub(crate) stream_idle_secs: Option<u64>,
    pub(crate) repository_url: String,
    pub(crate) repository_http_client: reqwest::Client,
}

impl AgentRunner {
    pub(crate) fn new(
        provenance_config: ProvenanceConfig,
        deployment_state: Arc<crate::deployment_state::DeploymentStateStore>,
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

    pub(crate) fn deployment_state(&self) -> &Arc<crate::deployment_state::DeploymentStateStore> {
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

    pub(crate) fn internal_a2a_router(&self) -> &Arc<InternalA2aRouter> {
        &self.internal_a2a_router
    }

    #[allow(dead_code)]
    pub(crate) fn insert_agent(&self, name: String, route_key: AgentRouteKey, booted: BootedAgent) {
        {
            let mut routed = self.routed_agents.write().expect("RwLock poison");
            routed.insert(route_key, booted.agent.clone());
        }
        let mut guard = self.agents.write().expect("RwLock poison");
        guard.insert(name.clone(), booted);
        let count = guard.len();
        drop(guard);
        tracing::info!(agent = %name, total_agents = count, "Runner: agent inserted");
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
                warn!(
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

    pub(crate) async fn handle_a2a_by_key(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let routed_agent = {
            let routed_agents = self.routed_agents.read().expect("RwLock poison");
            routed_agents.get(key).cloned().ok_or_else(|| {
                BamlRtError::AgentNotFound(format!(
                    "Agent {}/{} not found",
                    key.agent_package.as_str(),
                    key.agent_instance_id.as_str()
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
                    "Agent {}/{} not found",
                    key.agent_package.as_str(),
                    key.agent_instance_id.as_str()
                ))
            })?
        };
        routed_agent.handle_dispatch(request).await
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
                Ok(stream) => baml_rt_core::collect_a2a_stream(stream)
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
                .any(|agent| agent.content_hash.as_ref() == Some(content_hash))
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

        let (name, route_key, booted) = self
            .boot_from_blob_bytes(&bytes, content_hash, Some(repository_version))
            .await?;

        {
            let mut agents = self.agents.write().expect("RwLock poison");
            if agents
                .values()
                .any(|agent| agent.content_hash.as_ref() == Some(content_hash))
            {
                return Ok(baml_rt_core::DeployResult {
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

        Ok(baml_rt_core::DeployResult {
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
                let rk = AgentRouteKey::new(package_name, AgentInstanceId::default());
                let mut routed = self.routed_agents.write().expect("RwLock poison");
                routed.remove(&rk);
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
