//! BAML runtime core types and shared utilities.

pub mod a2a_handler;
pub mod a2a_sse;
pub mod a2a_wire;
pub mod agent_routing;
pub mod atomic_io;
pub mod backoff;
pub mod bus;
pub(crate) mod bus_spans;
pub mod callback_store;
pub mod clock_events;
pub mod context;
pub mod conversation_history;
pub mod correlation;
pub mod deferred;
pub mod deployment;
pub mod dispatch;
pub mod effect_metrics;
pub mod error;
pub mod event_producer;
pub mod event_subscription;
pub mod function_id;
pub mod ids;
pub mod ingress_store;
pub mod json;
pub mod package;
pub mod progress_probe;
pub mod retry_after;
pub mod semantics;
pub mod serde_one_or_many;
pub mod stream_completion;
pub mod time;
pub mod types;

pub use a2a_handler::{
    A2aJsChatHost, A2aRequestHandler, collect_a2a_stream, collect_a2a_stream_until,
};
pub use a2a_sse::{A2aSseParseError, parse_a2a_sse_json_rpc_chunks};
pub use a2a_wire::{A2aStreamChunk, A2aWireRequest};
pub use agent_routing::{
    AgentCard, AgentDiscoveryEntry, AgentInstanceId, AgentLister, AgentPackageName, AgentRouteKey,
    route_key_from_request,
};
pub use backoff::ExponentialBackoff;
pub use baml_rt_citation::Citation;
pub use bus::{
    A2aEffectMetadata, A2aKind, Bus, BusApi, BusStream, BusWithEffects, Command, DomainEvent,
    EffectEmitter, EffectEvent, EffectKind, EffectLiveness, EffectRuntime, EffectStartToken,
    EffectSubscriber, EffectSubscriberTier, Envelope, InFlightCounts, LlmEffectMetadata, LlmKind,
    LlmUsage, Payload, Subscriber, ToolEffectMetadata, ToolKind,
};
pub use callback_store::{
    CallbackDeliveryGate, CallbackStore, CancelCallbackSelector, ScheduleCallbackRequest,
    ScheduleCallbackResult, StoredCallback, callback_store_not_installed,
};
pub use context::{
    InvocationContext, InvocationScope, OutcomeInvocationContext, RequestScope, RuntimeScope,
    Scoped,
};
pub use conversation_history::ConversationHistoryUpdate;
pub use deferred::DeferredHolder;
pub use deployment::{
    DeployResult, DeploymentContentHash, DeploymentManager, DeploymentRecord, DeploymentStatus,
    UndeployResult,
};
pub use dispatch::{
    AgentDispatchAck, AgentDispatchRequest, AgentDispatchRoutingKey,
    DISPATCH_METADATA_SCHEDULING_CONTEXT_ID, DISPATCH_METADATA_SCHEDULING_TASK_ID,
    callback_scheduling_scopes_differ_from_dispatch, invocation_scope_for_agent_dispatch,
    scheduling_scope_from_dispatch_metadata,
};
pub use error::{
    BamlRtError, ClassifiedToolError, HeartbeatErrorKind, Result, SessionLifecycleError,
    baml_error_disposition, retryability_for_a2a,
};
pub use event_producer::{EventDeliveryOutcome, ProducedEvent};
pub use event_subscription::{
    EventSchemaVersion, EventSourceKind, EventSubscription, EventSubscriptionFilter,
    subscriptions_match_filter,
};
pub use function_id::{BamlFunctionId, BamlPromptName, VariantPhase};
pub use ids::{
    ActivityAnchorId, AgentId, ArtifactId, ContextId, CorrelationId, ExecutionSessionId, IntentId,
    MessageId, PlanId, PlanStepId, TaskId,
};
pub use ingress_store::{IngressId, IngressItem, IngressStore, ingress_store_not_installed};
pub use json::to_json_value;
pub use package::AgentManifest;
pub use progress_probe::{ProgressProbe, ProgressProbeRegistry, register_progress_probe};
pub use retry_after::{RetryAfter, parse_retry_after};
pub use semantics::{ActivityOutcome, ErrorDisposition, InvocationKind, Outcome, Retryability};
pub use stream_completion::{StreamCompletion, StreamResult};
pub use time::{now_unix_ms, now_unix_secs};
