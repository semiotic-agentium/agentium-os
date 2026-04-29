//! HTTP API surface for BAML agent routing and discovery.
//!
//! Exposes strict routes `/agents` (discovery) and `/agents/{agent_package}/{agent_instance_id}/a2a`
//! (SSE streaming JSON-RPC responses).
//! (A2A JSON-RPC forward). Delegates to an internal [`AgentRegistry`](baml_rt_a2a::AgentRegistry) implementation.

mod config_handlers;
mod context_index;
mod context_metrics;
mod conversation_history;
pub mod episode;
mod handlers;
mod mermaid;
mod metrics;
mod openapi;
mod otel_middleware;
mod planning;
mod provenance_ops;
mod repository_publish;
mod router;
pub mod service_error;
mod spans;

pub use context_index::{
    ContextIndexCursorToken, ContextIndexError, ContextIndexQueryParams, ContextIndexRequest,
    ContextIndexRequestParseError, ContextIndexService, ContextPickerItemDto, ContextPickerPageDto,
};
pub use context_metrics::{
    ContextMetricsError, ContextMetricsResponseDto, ContextMetricsService,
    ContextSessionMetricsDto, ContextTurnMetricsDto, TokenUsageDto,
};
pub use conversation_history::{
    ConversationHistoryContentDto, ConversationHistoryDeltaRequest, ConversationHistoryError,
    ConversationHistoryEventService, ConversationHistoryFormat, ConversationHistoryItemDto,
    ConversationHistoryPageDto, ConversationHistoryPageRequest, ConversationHistoryProfile,
    ConversationHistoryQueryParams, ConversationHistoryRequest,
    ConversationHistoryRequestParseError, ConversationHistoryService, ConversationHistoryUpdate,
    CursorToken, SessionStepOpDto, ToolOutcomeDto, page_version, paginate_items, profile_filter,
};
pub use episode::{
    ArtifactSummaryDto, EpisodeContentDto, EpisodeDurationDto, EpisodeEntryDto, EpisodeError,
    EpisodeOutcomeDto, EpisodeService, EpisodeSnapshotDto, IntentRevisionDto, PlanRevisionDto,
    PlanStepEntryDto, StepTypeDto, TerminalStatusDto, TokenSummaryDto,
};
pub use mermaid::{MermaidError, MermaidService};
pub use planning::{
    CitationDetail, ContextPlanningResponse, DriftedCallDetail, PlanningError, PlanningService,
    PlanningStepSummary, TaskPlanDriftSummary, TaskPlanningSnapshot,
};
pub use provenance_ops::{ProvenanceOpsError, ProvenanceOpsService};
pub use router::{
    ApiState, ClusterMode, api_router, api_router_with_services,
    api_router_with_services_and_deploy, serve, serve_with_services,
    serve_with_services_and_deploy,
};
pub use service_error::{ServiceError, service_result_to_http};
