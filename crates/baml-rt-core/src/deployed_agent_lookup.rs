//! Resolve deployed route keys to live booted [`AgentId`] values.

use async_trait::async_trait;

use crate::{agent_routing::AgentRouteKey, ids::AgentId};

/// Maps `(agent_package, agent_instance_id)` to the live runtime id from deploy routing.
#[async_trait]
pub trait DeployedAgentLookup: Send + Sync {
    fn agent_id_for_route(&self, route: &AgentRouteKey) -> Option<AgentId>;
}
