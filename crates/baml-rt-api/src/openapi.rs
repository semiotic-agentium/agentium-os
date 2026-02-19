//! OpenAPI schema types and spec builder.

use serde::Serialize;
use utoipa::ToSchema;

/// Cut-down agent card (included in every GET /agents item).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentCardDto {
    pub name: String,
    pub version: String,
    pub agent_package: String,
    pub agent_instance_id: String,
    pub tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub capabilities: Vec<String>,
}

/// Discovery entry for one running agent (GET /agents response item).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentDiscoveryEntryDto {
    pub agent_package: String,
    pub agent_instance_id: String,
    pub name: String,
    pub version: String,
    /// Agent card (cut-down shape) for discovery.
    pub agent_card: AgentCardDto,
}

impl From<baml_rt_core::AgentCard> for AgentCardDto {
    fn from(c: baml_rt_core::AgentCard) -> Self {
        Self {
            name: c.name,
            version: c.version,
            agent_package: c.agent_package,
            agent_instance_id: c.agent_instance_id,
            tools: c.tools,
            description: c.description,
            capabilities: c.capabilities,
        }
    }
}

impl From<baml_rt_core::AgentDiscoveryEntry> for AgentDiscoveryEntryDto {
    fn from(e: baml_rt_core::AgentDiscoveryEntry) -> Self {
        Self {
            agent_package: e.agent_package,
            agent_instance_id: e.agent_instance_id,
            name: e.name,
            version: e.version,
            agent_card: AgentCardDto::from(e.agent_card),
        }
    }
}
