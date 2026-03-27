//! In-process A2A routing and registry adapters.

use std::sync::Arc;

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

/// Routes in-process A2A requests to loaded agents.
/// Holds an `OnceLock` back-pointer to the runner set after construction.
#[derive(Clone)]
pub(crate) struct InternalA2aRouter {
    runner: std::sync::OnceLock<Arc<AgentRunner>>,
}

impl InternalA2aRouter {
    pub(crate) fn new() -> Self {
        Self {
            runner: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn set_runner(&self, runner: Arc<AgentRunner>) {
        if self.runner.set(runner).is_err() {
            tracing::warn!(
                "InternalA2aRouter::set_runner called after runner already set; duplicate wiring ignored"
            );
        }
    }

    pub(crate) async fn route_from(
        &self,
        caller: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        let runner = self
            .runner
            .get()
            .expect("InternalA2aRouter: runner not set");

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
                "system/internal_a2a only supports agent_instance_id=default, got '{}'",
                key.agent_instance_id.as_str()
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
        Err(_) => InvocationScope::synthetic_message(agent_id),
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
