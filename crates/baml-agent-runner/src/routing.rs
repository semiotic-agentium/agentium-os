//! In-process A2A routing and registry adapters.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use baml_rt_a2a::{A2aRequestHandler, AgentRegistry, a2a};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentInstanceId, AgentLister,
    AgentPackageName, AgentRouteKey, BamlRtError, Result,
    bus::BusStream,
    context::{self, InvocationScope},
    ids::AgentId,
    route_key_from_request,
};
use serde_json::Value;

use crate::runner::AgentRunner;

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

/// Dynamic discovery list for [`crate::agent_package::AgentPackage::boot`]: each `list_agents`
/// reflects the **current** deployed registry (same data as HTTP `GET /agents`).
///
/// Uses [`Weak`] so the tool registry does not retain a strong `Arc<AgentRunner>` cycle
/// with the booted agent.
#[derive(Clone)]
pub(crate) struct LiveAgentLister {
    runner: Weak<AgentRunner>,
}

impl LiveAgentLister {
    pub(crate) fn new(runner: Weak<AgentRunner>) -> Self {
        Self { runner }
    }
}

impl AgentLister for LiveAgentLister {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let Some(runner) = self.runner.upgrade() else {
            tracing::warn!("LiveAgentLister: runner host dropped; returning empty discovery list");
            return Vec::new();
        };
        let entries = runner.discovery_entries();
        tracing::debug!(
            count = entries.len(),
            "LiveAgentLister list_agents (dynamic, matches HTTP GET /agents)"
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

/// Placement of an agent on a remote runner: the routable HTTP endpoint plus
/// the canonical OTEL `service.instance.id` of the serving runner — what the
/// ingress side stamps as `target_service_instance_id` on forwarded spans and
/// metrics. Not `RunnerId` (internal cluster UUID) and not
/// `cluster_runners.pod_name` (HOSTNAME-derived; may diverge from the OTEL
/// identity when an operator sets `OTEL_RESOURCE_ATTRIBUTES`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) endpoint: String,
    pub(crate) service_instance_id: String,
}

/// Resolves an agent route key to the placement of the runner hosting it.
/// Returns `Ok(None)` if the agent is unknown to the cluster, or `Err` on transient failures.
#[async_trait]
pub(crate) trait ClusterEndpointResolver: Send + Sync {
    async fn resolve(&self, key: &AgentRouteKey) -> Result<Option<Placement>>;
}

/// Routes A2A requests: local agents first, then cluster fallback via HTTP.
///
/// Cross-runner A2A forwarding is intentionally unauthenticated at the application
/// layer. Cluster security relies on network-level isolation (K8s NetworkPolicy,
/// service mesh mTLS). Control-plane endpoints (deploy, undeploy, migrate) are
/// separately gated by `require_control_token` in the API layer.
pub(crate) struct InternalA2aRouter {
    runner: std::sync::OnceLock<Arc<AgentRunner>>,
    cluster: std::sync::OnceLock<Arc<dyn ClusterEndpointResolver>>,
}

impl InternalA2aRouter {
    pub(crate) fn new() -> Self {
        Self {
            runner: std::sync::OnceLock::new(),
            cluster: std::sync::OnceLock::new(),
        }
    }

    /// Return the cluster endpoint resolver, if one has been configured.
    pub(crate) fn cluster_resolver(&self) -> Option<&Arc<dyn ClusterEndpointResolver>> {
        self.cluster.get()
    }

    /// Set the cluster endpoint resolver for cross-runtime routing.
    /// Called after runner construction when a shared SurrealDB is configured.
    pub(crate) fn set_cluster(&self, resolver: Arc<dyn ClusterEndpointResolver>) {
        if self.cluster.set(resolver).is_err() {
            tracing::warn!(
                "InternalA2aRouter::set_cluster called after cluster already set; ignored"
            );
        }
    }

    pub(crate) fn set_runner(&self, runner: Arc<AgentRunner>) {
        if self.runner.set(runner).is_err() {
            tracing::warn!(
                "InternalA2aRouter::set_runner called after runner already set; duplicate wiring ignored"
            );
        }
    }

    pub(crate) fn try_runner(&self) -> Option<Arc<AgentRunner>> {
        self.runner.get().cloned()
    }

    pub(crate) async fn route_from(
        &self,
        caller: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let runner = self.runner.get().ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "InternalA2aRouter: runner not set (route_from called before set_runner)".into(),
            )
        })?;

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
            let instance_id = key.agent_instance_id.as_str();
            return Err(BamlRtError::InvalidArgument(format!(
                "system/internal_a2a only supports agent_instance_id=default, got '{instance_id}'"
            )));
        }
        if key == *caller {
            return Err(BamlRtError::InvalidArgument(
                "system/internal_a2a self-routing is not allowed".to_string(),
            ));
        }

        // Local fast path: agent is on this runtime.
        let local_agent = {
            let agents = runner
                .routed_agents
                .read()
                .map_err(|_| BamlRtError::InvalidArgument("routed_agents lock poisoned".into()))?;
            agents.get(&key).cloned()
        };

        if let Some(routed_agent) = local_agent {
            let scope = scope_from_request(request.as_ref(), routed_agent.agent_id().clone());
            return context::with_scope(scope.as_scope().clone(), async move {
                routed_agent.handle_a2a_stream(request).await
            })
            .await;
        }

        // Cluster fallback: forward to the remote runner hosting this agent.
        if let Some(resolver) = self.cluster.get() {
            match resolver.resolve(&key).await {
                Ok(Some(placement)) => {
                    tracing::info!(
                        agent = %key.agent_package.as_str(),
                        endpoint = %placement.endpoint,
                        target_service_instance_id = %placement.service_instance_id,
                        "routing A2A request to remote runner"
                    );
                    return self.forward_to_runner(&placement, &key, request).await;
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
            "Agent {pkg}/{inst} not found locally or in cluster"
        )))
    }

    /// Forward an A2A request to a remote runner via HTTP POST and bridge the
    /// JSON response back as a stream of individual JSON-RPC chunks.
    ///
    /// The ingress runner's `service.instance.id` rides along so the serving
    /// runner can stamp `ingress_service_instance_id` on its spans / metrics
    /// via propagated OTEL baggage.
    pub(crate) async fn forward_to_runner(
        &self,
        placement: &Placement,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let pkg = key.agent_package.as_str();
        let inst = key.agent_instance_id.as_str();
        let target =
            baml_rt_router::forward::resolve_forward_target(&placement.endpoint, pkg, inst).await?;
        let body = request.into_inner();
        let ingress_service_instance_id = baml_rt_observability::service_instance_id();
        let items = baml_rt_router::forward::forward_request(
            &target,
            &body,
            &key.agent_package,
            &key.agent_instance_id,
            ingress_service_instance_id,
            Some(placement.service_instance_id.as_str()),
        )
        .await?;
        let chunks = response_body_to_chunks(items);
        Ok(Box::pin(futures_util::stream::iter(chunks)))
    }
}

/// Convert a list of JSON values from a forwarded HTTP response into individual stream chunks.
fn response_body_to_chunks(items: Vec<Value>) -> Vec<A2aStreamChunk> {
    items.into_iter().map(A2aStreamChunk::from).collect()
}

/// Caller-scoped wrapper for `InternalA2aRouter`.
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
pub(crate) fn scope_from_request(request: &Value, agent_id: AgentId) -> InvocationScope {
    match a2a::A2aRequest::from_value(request.clone()) {
        Ok(parsed) => InvocationScope::new(baml_rt_core::RuntimeScope::from_request_scope(
            &parsed.resolved_scope,
            agent_id,
        )),
        Err(e) => {
            tracing::warn!(error = %e, "scope_from_request: failed to parse A2A request, using synthetic scope");
            InvocationScope::synthetic_message(agent_id)
        }
    }
}

pub(crate) fn extract_internal_a2a_target(request: &Value) -> Option<AgentRouteKey> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_response_produces_individual_chunks() {
        let items: Vec<Value> = vec![
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"status": "working"}}),
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"status": "completed"}}),
        ];
        let chunks = response_body_to_chunks(items);
        assert_eq!(
            chunks.len(),
            2,
            "each JSON-RPC object should be a separate chunk"
        );
    }

    #[test]
    fn single_object_wrapped_produces_one_chunk() {
        let items: Vec<Value> = vec![serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {}})];
        let chunks = response_body_to_chunks(items);
        assert_eq!(chunks.len(), 1);
    }
}
