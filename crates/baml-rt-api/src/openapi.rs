//! OpenAPI schema types and spec builder.

use serde::Serialize;
use utoipa::ToSchema;

/// Discovery entry for one running agent (GET /agents response item).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentDiscoveryEntryDto {
    pub agent_package: String,
    pub agent_instance_id: String,
    pub name: String,
    pub version: String,
}

impl From<baml_rt_core::AgentDiscoveryEntry> for AgentDiscoveryEntryDto {
    fn from(e: baml_rt_core::AgentDiscoveryEntry) -> Self {
        Self {
            agent_package: e.agent_package,
            agent_instance_id: e.agent_instance_id,
            name: e.name,
            version: e.version,
        }
    }
}
