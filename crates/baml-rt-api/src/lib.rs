//! HTTP API surface for BAML agent routing and discovery.
//!
//! Exposes strict routes `/agents` (discovery) and `/agents/{agent_package}/{agent_instance_id}/a2a`
//! (A2A JSON-RPC forward). Delegates to an internal [`AgentRegistry`](baml_rt_a2a::AgentRegistry) implementation.

mod handlers;
mod metrics;
mod openapi;
mod router;
mod spans;

pub use router::ApiState;
pub use router::{api_router, serve};
