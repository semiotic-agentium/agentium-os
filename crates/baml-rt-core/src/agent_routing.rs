//! Agent routing identity: package + instance id for strict HTTP path routing.
//! Used by the runner registry and the HTTP API surface.

use serde::{Deserialize, Serialize};

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

/// Discovery entry for one running agent instance (GET /agents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiscoveryEntry {
    pub agent_package: String,
    pub agent_instance_id: String,
    /// Manifest name (human-readable).
    pub name: String,
    pub version: String,
}
