// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! HTTP API surface for BAML agent routing and discovery.
//!
//! Exposes strict routes `/agents` (discovery) and `/agents/{agent_package}/{agent_instance_id}/a2a`
//! (SSE streaming JSON-RPC responses).
//! (A2A JSON-RPC forward). Delegates to an internal [`AgentRegistry`](baml_rt_a2a::AgentRegistry) implementation.

pub mod cluster_agents;
pub mod cluster_deploy;
pub mod cluster_heartbeat;
mod config_handlers;
mod context_index;
mod context_metrics;
mod conversation_history;
pub mod episode;
pub mod event_console;
mod handlers;
mod mermaid;
mod metrics;
mod openapi;
mod otel_middleware;
mod planning;
mod provenance_ops;
mod repository_publish;
mod router;
pub mod runtime_progress;
pub mod service_error;
mod spans;
pub mod webhook_mount;

pub use baml_rt_core::HeartbeatErrorKind;
pub use cluster_agents::{
    ClusterAgentPlacementDto, ClusterAgentRowDto, ClusterAgentsResponseDto, ClusterDirectoryError,
    ClusterDirectoryService, ClusterPlacementInfo, ClusterRunnerInfo, ClusterRunnerStatusDto,
    PlacementSourceDto,
};
pub use cluster_deploy::{ClusterDeployResponseDto, ClusterDeployRunnerResultDto};
pub use cluster_heartbeat::{ClusterHeartbeatHealth, HeartbeatStatus};
pub use context_index::{
    ContextIndexCursorToken, ContextIndexError, ContextIndexQueryParams, ContextIndexRequest,
    ContextIndexRequestParseError, ContextIndexService, ContextPickerIngressFilter,
    ContextPickerItemDto, ContextPickerPageDto,
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
    CursorToken, DEFAULT_CONVERSATION_HISTORY_LIMIT, LlmPromptOperationDto, SessionStepOpDto,
    ToolOutcomeDto, apply_conversation_history_profile, include_in_conversation_history_profile,
    page_from_transcript_slice, page_version, profile_filter,
};
pub use episode::{
    ArtifactSummaryDto, EpisodeContentDto, EpisodeDurationDto, EpisodeEntryDto, EpisodeError,
    EpisodeOutcomeDto, EpisodeService, EpisodeSnapshotDto, IntentRevisionDto, PlanRevisionDto,
    PlanStepEntryDto, StepTypeDto, TerminalStatusDto, TokenSummaryDto,
};
pub use mermaid::{MermaidError, MermaidService};
pub use planning::{
    CitationDetail, ContextPlanningResponse, DriftedCallDetail, PlanningError, PlanningService,
    PlanningStepSummary, TaskPlanDriftSummary, TaskPlanningSnapshot, summarize_plan_steps,
};
pub use provenance_ops::{ProvenanceOpsError, ProvenanceOpsService};
pub use router::{
    ApiServerConfig, ApiState, ClusterMode, ClusterTopology, LISTENER_EXIT_AFTER_SECS_ENV,
    api_router, api_router_with_services_and_deploy, serve_with_services_and_deploy,
};
pub use runtime_progress::{READYZ_LAG_THRESHOLD_MS, RuntimeProgressMeter};
pub use service_error::{ServiceError, service_result_to_http};
pub use webhook_mount::{WebhookIntakeRouters, build_webhook_intake_router};
