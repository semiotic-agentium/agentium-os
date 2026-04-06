//! Agent routing identity and discovery catalogue.
//!
//! - [`keys`]: typed package/instance identifiers, [`AgentRouteKey`], [`route_key_from_request`].
//! - [`discovery`]: [`AgentCard`], [`AgentDiscoveryEntry`], [`AgentLister`] for GET /agents and tools.

mod discovery;
mod keys;

pub use discovery::{AgentCard, AgentDiscoveryEntry, AgentLister};
pub use keys::{AgentInstanceId, AgentPackageName, AgentRouteKey, route_key_from_request};
