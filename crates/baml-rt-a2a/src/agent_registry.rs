// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Internal agent routing surface: listing and dispatching A2A by route key.
//! Consumed by the HTTP API surface (baml-rt-api); implemented by the runner.

use async_trait::async_trait;
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDispatchAck, AgentDispatchRequest, AgentLister,
    AgentRouteKey, BusStream, Result,
};

/// Registry of running agents: list and dispatch A2A by strict route key.
/// Implemented by the runner; consumed by the HTTP API surface.
#[async_trait]
pub trait AgentRegistry: AgentLister + Send + Sync {
    /// Resolve an agent by route key and handle an A2A JSON-RPC request.
    /// Returns 404-equivalent when the key is unknown (caller maps to HTTP 404).
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>>;

    /// Resolve an agent by route key and deliver a deterministic non-conversational dispatch.
    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck>;
}
