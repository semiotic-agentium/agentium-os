//! BAML runtime core types and shared utilities.

pub mod a2a_handler;
pub mod agent_routing;
pub mod bus;
pub mod context;
pub mod correlation;
pub mod error;
pub mod ids;
pub mod json;
pub mod package;
pub mod semantics;
pub mod stream_completion;
pub mod types;

pub use a2a_handler::{A2aRequestHandler, collect_a2a_stream, collect_a2a_stream_until};
pub use agent_routing::{AgentDiscoveryEntry, AgentRouteKey};
pub use bus::{
    A2aEffectMetadata, A2aKind, EffectEmitter, EffectEvent, EffectKind, EffectLiveness,
    EffectStartToken, EffectSubscriber, InFlightCounts, LlmEffectMetadata, LlmKind, LlmUsage,
    ToolEffectMetadata, ToolKind,
};
pub use bus::{
    Bus, BusApi, BusStream, BusWithEffects, Command, DomainEvent, EffectRuntime, Envelope, Payload,
    Subscriber,
};
pub use context::{InvocationContext, InvocationScope, RuntimeScope, Scoped};
pub use error::{BamlRtError, Result};
pub use ids::{AgentId, ArtifactId, ContextId, CorrelationId, EventId, MessageId, TaskId};
pub use json::to_json_value;
pub use package::AgentManifest;
pub use semantics::{InvocationKind, Outcome, Retryability};
pub use stream_completion::{StreamCompletion, StreamResult};
