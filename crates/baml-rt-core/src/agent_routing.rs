//! Agent routing identity: package + instance id for strict HTTP path routing.
//! Used by the runner registry and the HTTP API surface.

use crate::{BamlRtError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Route key for an agent instance: agent_package (e.g. manifest name) + agent_instance_id.
/// Used in HTTP paths: `/agents/{agent_package}/{agent_instance_id}/...`
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentRouteKey {
    pub agent_package: String,
    pub agent_instance_id: String,
}

impl AgentRouteKey {
    pub fn new(agent_package: impl Into<String>, agent_instance_id: impl Into<String>) -> Self {
        Self {
            agent_package: agent_package.into(),
            agent_instance_id: agent_instance_id.into(),
        }
    }
}

/// Cut-down A2A-like agent card for discovery (included in every GET /agents entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    /// Route key for A2A dispatch.
    pub agent_package: String,
    pub agent_instance_id: String,
    /// Tool names declared in manifest.
    #[serde(default)]
    pub tools: Vec<String>,
    /// From manifest.discovery when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Narrow trait: lists running agents. HTTP GET /agents and system/discover_agents depend on this.
pub trait AgentLister: Send + Sync {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry>;
}

/// Holder for the agent-list catalogue (legacy/compatibility).
/// Prefer injecting a concrete `AgentLister` (e.g. registry) at construction time so
/// discovery never runs with an unset provider. If used, the host must call `set()`
/// before any call to `list_agents()`; otherwise this implementation panics with a clear message.
pub struct AgentListCatalogueHolder {
    inner: crate::deferred::DeferredHolder<dyn AgentLister>,
}

impl AgentListCatalogueHolder {
    pub fn new() -> Self {
        Self {
            inner: crate::deferred::DeferredHolder::new(),
        }
    }

    pub fn set(&self, provider: std::sync::Arc<dyn AgentLister>) {
        self.inner.set(provider);
    }
}

impl Default for AgentListCatalogueHolder {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentLister for AgentListCatalogueHolder {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.inner
            .get()
            .expect("AgentListCatalogueHolder not set: host must call set() before list_agents()")
            .list_agents()
    }
}

/// Discovery entry for one running agent instance (GET /agents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiscoveryEntry {
    pub agent_package: String,
    pub agent_instance_id: String,
    /// Manifest name (human-readable).
    pub name: String,
    pub version: String,
    /// Agent card (cut-down shape) for discovery.
    pub agent_card: AgentCard,
}

/// Extract route key from a JSON-RPC A2A request (params.metadata.target from system/internal_a2a).
/// Centralizes the protocol so all consumers use the same parsing.
pub fn route_key_from_request(request: &Value) -> Result<AgentRouteKey> {
    let params = request
        .get("params")
        .and_then(|p| p.as_object())
        .ok_or_else(|| BamlRtError::InvalidArgument("params must be an object".to_string()))?;
    let meta = params
        .get("metadata")
        .and_then(|m| m.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("params.metadata required for routing".to_string())
        })?;
    let target = meta
        .get("target")
        .and_then(|t| t.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("params.metadata.target required".to_string())
        })?;
    let agent_package = target
        .get("agent_package")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "params.metadata.target.agent_package required".to_string(),
            )
        })?
        .to_string();
    let agent_instance_id = target
        .get("agent_instance_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    Ok(AgentRouteKey::new(agent_package, agent_instance_id))
}

#[cfg(test)]
mod tests {
    use super::{AgentListCatalogueHolder, AgentLister};

    /// Construction-order invariant: list_agents() panics when holder was never set.
    #[test]
    #[should_panic(expected = "AgentListCatalogueHolder not set")]
    fn catalogue_holder_list_agents_panics_when_unset() {
        let holder = AgentListCatalogueHolder::new();
        let _ = holder.list_agents();
    }
}
