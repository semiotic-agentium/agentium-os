//! HTTP API surface for BAML agent routing and discovery.
//!
//! Exposes strict routes `/agents` (discovery) and `/agents/{agent_package}/{agent_instance_id}/a2a`
//! (A2A JSON-RPC forward). Delegates to an internal [`AgentRegistry`](baml_rt_a2a::AgentRegistry) implementation.

mod context_metrics;
mod handlers;
mod mermaid;
mod metrics;
mod openapi;
mod router;
mod spans;

pub use context_metrics::{
    ContextMetricsError, ContextMetricsResponseDto, ContextMetricsService,
    ContextSessionMetricsDto, ContextTurnMetricsDto, TokenUsageDto,
};
pub use mermaid::{MermaidError, MermaidService};
pub use router::{ApiState, api_router, api_router_with_services, serve, serve_with_services};
