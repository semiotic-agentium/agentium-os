//! Internal agent routing surface: listing and dispatching A2A by route key.
//! Consumed by the HTTP API surface (baml-rt-api); implemented by the runner.

use async_trait::async_trait;
use baml_rt_core::{AgentDiscoveryEntry, AgentRouteKey, BusStream, Result};
use serde_json::Value;

/// Registry of running agents: list and dispatch A2A by strict route key.
/// Implemented by the runner; consumed by the HTTP API surface.
#[async_trait]
pub trait AgentRegistry: Send + Sync {
    /// List all running agent instances (for GET /agents discovery).
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry>;

    /// Resolve an agent by route key and handle an A2A JSON-RPC request.
    /// Returns 404-equivalent when the key is unknown (caller maps to HTTP 404).
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: Value,
    ) -> Result<BusStream<Value>>;
}
