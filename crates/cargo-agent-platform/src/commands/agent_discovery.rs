use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AgentDiscoveryEntry {
    pub agent_card: AgentCard,
}

#[derive(Debug, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub agent_package: String,
    pub agent_instance_id: String,
}
