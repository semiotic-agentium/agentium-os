//! BAML runtime core types and shared utilities.

pub mod a2a_handler;
pub mod a2a_wire;
pub mod agent_routing;
pub mod bus;
pub mod context;
pub mod correlation;
pub mod deferred;
pub mod error;
pub mod event_subscription;
pub mod ids;
pub mod json;
pub mod package;
pub mod semantics;
pub mod stream_completion;
pub mod types;

pub use a2a_handler::{A2aRequestHandler, collect_a2a_stream, collect_a2a_stream_until};
pub use a2a_wire::{A2aStreamChunk, A2aWireRequest};
pub use agent_routing::{
    AgentCard, AgentDiscoveryEntry, AgentInstanceId, AgentListCatalogueHolder, AgentLister,
    AgentPackageName, AgentRouteKey, route_key_from_request,
};
pub use bus::{
    A2aEffectMetadata, A2aKind, Bus, BusApi, BusStream, BusWithEffects, Command, DomainEvent,
    EffectEmitter, EffectEvent, EffectKind, EffectLiveness, EffectRuntime, EffectStartToken,
    EffectSubscriber, Envelope, InFlightCounts, LlmEffectMetadata, LlmKind, LlmUsage, Payload,
    Subscriber, ToolEffectMetadata, ToolKind,
};
pub use context::{
    InvocationContext, InvocationScope, OutcomeInvocationContext, RequestScope, RuntimeScope,
    Scoped,
};
pub use deferred::DeferredHolder;
pub use error::{BamlRtError, Result, SessionLifecycleError};
pub use event_subscription::{
    EventSchemaVersion, EventSourceKey, EventSourceKeyPrefix, EventSourceKind, EventSubscription,
    EventSubscriptionFilter, PublishedEvent, subscriptions_match_filter,
    subscriptions_match_published_event,
};
pub use ids::{
    AgentId, ArtifactId, ContextId, CorrelationId, EventId, ExecutionSessionId, IntentId,
    MessageId, PlanId, PlanStepId, TaskId,
};
pub use json::to_json_value;
pub use package::AgentManifest;
pub use semantics::{ActivityOutcome, InvocationKind, Outcome, Retryability};
pub use stream_completion::{StreamCompletion, StreamResult};
