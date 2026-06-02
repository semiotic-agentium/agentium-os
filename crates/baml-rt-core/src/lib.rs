// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! BAML runtime core types and shared utilities.

pub mod a2a_handler;
pub mod a2a_sse;
pub mod a2a_wire;
pub mod agent_routing;
pub mod atomic_io;
pub mod backoff;
pub mod blocking_task;
pub mod bus;
pub(crate) mod bus_spans;
pub mod callback_store;
pub mod clock_events;
pub mod context;
pub mod correlation;
pub mod deferred;
pub mod deployed_agent_lookup;
pub mod deployment;
pub mod dispatch;
pub mod dispatch_ingress;
pub mod effect_metrics;
pub mod error;
pub mod event_delivery;
pub mod event_producer;
pub mod event_subscription;
pub mod function_id;
pub mod history_text;
pub mod host_ingress_recorder;
pub mod host_poll_lineage;
pub mod host_source_records_body;
pub mod host_wire;
pub mod ids;
pub mod ingress_store;
pub mod json;
pub mod observation;
pub mod package;
pub mod progress_probe;
pub mod retry_after;
pub mod semantics;
pub mod serde_one_or_many;
pub mod step_executor_outcome;
pub mod stream_completion;
pub mod time;
pub mod types;

pub use a2a_handler::{
    A2aJsChatHost, A2aRequestHandler, collect_a2a_stream_one_shot,
    collect_a2a_stream_until_one_shot,
};
pub use a2a_sse::{A2aSseDecoder, A2aSseParseError, parse_a2a_sse_json_rpc_chunks};
pub use a2a_wire::{A2aStreamChunk, A2aWireRequest};
pub use agent_routing::{
    AgentCard, AgentDiscoveryEntry, AgentInstanceId, AgentLister, AgentPackageName, AgentRouteKey,
    DispatchTarget, route_key_from_request,
};
pub use backoff::ExponentialBackoff;
pub use baml_rt_citation::Citation;
pub use blocking_task::join_error_message;
pub use bus::{
    A2aEffectMetadata, A2aKind, Bus, BusApi, BusStream, BusWithEffects, Command, DomainEvent,
    EffectEmitter, EffectEvent, EffectKind, EffectLiveness, EffectRuntime, EffectStartToken,
    EffectSubscriber, EffectSubscriberTier, Envelope, InFlightCounts, LlmEffectMetadata, LlmKind,
    LlmUsage, Payload, Subscriber, ToolEffectMetadata, ToolKind, bus_stream_channel,
};
pub use callback_store::{
    CallbackDeliveryGate, CallbackStore, CancelCallbackSelector, ScheduleCallbackRequest,
    ScheduleCallbackResult, StoredCallback, callback_store_not_installed,
};
pub use context::{
    InvocationContext, InvocationScope, OutcomeInvocationContext, RequestScope, RuntimeScope,
    Scoped,
};
pub use deferred::DeferredHolder;
pub use deployed_agent_lookup::DeployedAgentLookup;
pub use deployment::{
    DeployResult, DeploymentContentHash, DeploymentManager, DeploymentRecord, DeploymentStatus,
    UndeployResult,
};
pub use dispatch::{
    AgentDispatchAck, AgentDispatchRequest, AgentDispatchRoutingKey,
    DISPATCH_METADATA_SCHEDULING_CONTEXT_ID, DISPATCH_METADATA_SCHEDULING_TASK_ID,
    DispatchMetadata, callback_scheduling_scopes_differ_from_dispatch,
    invocation_scope_for_agent_dispatch, scheduling_scope_from_dispatch_metadata,
};
pub use dispatch_ingress::{
    DispatchUnitKey, DispatchWorkUnit, WithTaskPrelude, dispatch_unit_message_id,
    dispatch_unit_runtime_scope, dispatch_unit_task_id, format_unit_ingress_body,
};
pub use error::{
    BamlRtError, ClassifiedToolError, HeartbeatErrorKind, Result, SessionLifecycleError,
    baml_error_disposition, retryability_for_a2a,
};
pub use event_delivery::{
    AgentDispatchPort, DiscoveryPublishClient, HostPublishClient, HostPublishService,
    SubscriberIndex, deliver_to_subscribers, matching_subscriber_routes, publish_to_subscribers,
};
pub use event_producer::{
    EventDeliveryOutcome, ProducedEvent, SubscriberAcceptance, SubscriberDeliveryFailure,
};
pub use event_subscription::{
    EventSchemaVersion, EventSourceKind, EventSubscription, EventSubscriptionFilter,
    subscriptions_match_filter,
};
pub use function_id::{BamlFunctionId, BamlPromptName, VariantPhase};
pub use history_text::is_history_infrastructure_notice;
pub use host_ingress_recorder::{HostIngressRecorder, IngressPollUserMessageRef};
pub use host_poll_lineage::{
    HostPollLineage, PollLineageSeed, mint_host_poll_lineage, poll_batch_message_id,
};
pub use host_source_records_body::{
    IngressPollBody, format_source_records_message_body, format_source_records_unit_body,
};
pub use host_wire::{
    HostSourceDescriptor, HostSourceRecordsEnvelopeHeader, host_source_records_schema_version,
    wire as host_wire_versions,
};
pub use ids::{
    ActivityAnchorId, AgentId, ArtifactId, ContextId, CorrelationId, ExecutionSessionId, IntentId,
    MessageId, PlanId, PlanStepId, TaskId,
};
pub use ingress_store::{IngressId, IngressItem, IngressStore, ingress_store_not_installed};
pub use json::to_json_value;
pub use observation::{ObservationUpdate, kinds as ObservationKinds};
pub use package::AgentManifest;
pub use progress_probe::{ProgressProbe, ProgressProbeRegistry, register_progress_probe};
pub use retry_after::{RetryAfter, parse_retry_after};
pub use semantics::{ActivityOutcome, ErrorDisposition, InvocationKind, Outcome, Retryability};
pub use step_executor_outcome::{
    MAX_STEP_PLAN_FIX_STEPS, StepExecutorOutcome, StepPlanRecovery, StepPlanViolationCode,
};
pub use stream_completion::{StreamCompletion, StreamResult};
pub use time::{now_unix_ms, now_unix_secs};
