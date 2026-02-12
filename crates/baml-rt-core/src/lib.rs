//! BAML runtime core types and shared utilities.

pub mod agent_routing;
pub mod context;
pub mod correlation;
pub mod effects;
pub mod error;
pub mod ids;
pub mod json;
pub mod package;
pub mod types;

pub use agent_routing::{AgentDiscoveryEntry, AgentRouteKey};
pub use context::{InvocationContext, InvocationScope, RuntimeScope, Scoped};
pub use effects::{
    EffectBus, EffectEmitter, EffectEvent, EffectKind, EffectLiveness, EffectSubscriber,
    InFlightCounts,
};
pub use error::{BamlRtError, Result};
pub use ids::{AgentId, ArtifactId, ContextId, CorrelationId, EventId, MessageId, TaskId};
pub use json::to_json_value;
pub use package::AgentManifest;
