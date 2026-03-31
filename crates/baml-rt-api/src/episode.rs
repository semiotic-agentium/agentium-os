//! Episode service trait and response DTOs.

use async_trait::async_trait;
use serde::Serialize;
use utoipa::ToSchema;

/// Episode service errors — alias for the unified [`ServiceError`](crate::service_error::ServiceError).
pub type EpisodeError = crate::service_error::ServiceError;

/// Service that can produce an episode snapshot for a given task.
#[async_trait]
pub trait EpisodeService: Send + Sync {
    /// Return an episode snapshot (in-progress or terminal) for the given task id.
    async fn episode_snapshot(&self, task_id: &str) -> Result<EpisodeSnapshotDto, EpisodeError>;

    /// Return the canonical text rendering of the episode, produced by `render_episode`.
    async fn episode_text(&self, task_id: &str) -> Result<String, EpisodeError>;
}

/// Terminal task outcome for UI display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatusDto {
    Completed,
    Failed,
    Canceled,
    Rejected,
    #[serde(untagged)]
    Other(String),
}

impl TerminalStatusDto {
    /// True when the status represents a completed task lifecycle (not in-progress).
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

/// Task duration breakdown for UI display.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct EpisodeDurationDto {
    pub active_ms: u64,
    pub wait_ms: u64,
    pub wall_clock_ms: u64,
}

/// Token usage summary for UI display.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct TokenSummaryDto {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub llm_call_count: u32,
    pub llm_duration_ms: u64,
}

/// Episode step type for UI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepTypeDto {
    Message,
    ToolCall,
    ToolRead,
    ToolResult,
    PlanRevision,
    StatusTransition,
    ArtifactEmitted,
}

/// Episode entry content for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EpisodeContentDto {
    Text {
        text: String,
    },
    ToolInvocation {
        tool_name: String,
        description: String,
    },
    ToolOutput {
        tool_name: String,
        summary: String,
        line_count: usize,
        byte_count: usize,
        lines: Vec<String>,
    },
    PlanRevisionRef {
        summary: String,
    },
    StatusChange {
        old: String,
        new: String,
        message: Option<String>,
    },
    Artifact {
        name: String,
        media_type: Option<String>,
        size_bytes: Option<usize>,
    },
}

/// One line of session-style history (`conversation_history` mirror).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionHistoryLineDto {
    pub role: String,
    pub content: String,
}

/// Episode transcript entry for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EpisodeEntryDto {
    pub seq: u32,
    pub step_type: StepTypeDto,
    pub role: String,
    pub elapsed_ms: i64,
    pub content: EpisodeContentDto,
    pub activity_anchor: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citation_strings: Vec<String>,
}

/// Intent revision for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IntentRevisionDto {
    pub intent_id: String,
    pub description: String,
    pub activity_anchor: String,
    pub timestamp_ms: u64,
    pub superseded_by_next: bool,
    pub supersession_from_previous: Option<String>,
    pub derived_citation_strings: Vec<String>,
}

/// Plan step entry for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlanStepEntryDto {
    pub step_id: String,
    pub description: String,
    pub status: String,
    pub timestamp_ms: Option<u64>,
    pub citation_strings: Vec<String>,
}

/// Plan revision for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlanRevisionDto {
    pub plan_id: String,
    pub intent_id: String,
    pub activity_anchor: String,
    pub timestamp_ms: u64,
    pub superseded_by_next: bool,
    pub steps: Vec<PlanStepEntryDto>,
}

/// Artifact summary for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArtifactSummaryDto {
    pub name: String,
    pub media_type: Option<String>,
}

/// Episode outcome for UI display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EpisodeOutcomeDto {
    pub final_message: Option<String>,
    pub artifacts: Vec<ArtifactSummaryDto>,
    pub citation_strings: Vec<String>,
    pub token_summary: TokenSummaryDto,
    pub duration: EpisodeDurationDto,
}

/// Task-level drift summary for episodic evaluation and memory formation.
///
/// # Structural note
/// This type is field-for-field identical to [`baml_rt_provenance::EpisodeDriftSummary`].
/// They are kept separate so the domain crate remains free of API (`utoipa`) dependencies.
/// If you add a field to either type, you **must** add it to both and update the
/// `from_field_copy!` invocation below.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EpisodeDriftSummaryDto {
    pub composite_severity: String,
    pub intent_alignment: f32,
    pub step_alignment: Option<f32>,
    pub trajectory_drift: Option<f32>,
    pub plan_adherence_score: f32,
    pub scored_call_count: u32,
    pub warn_count: u32,
    pub block_count: u32,
}

/// Per-LLM-call drift detail anchored to transcript for episodic evaluation.
///
/// # Structural note
/// This type is field-for-field identical to [`baml_rt_provenance::EpisodeDriftCall`].
/// They are kept separate so the domain crate remains free of API (`utoipa`) dependencies.
/// If you add a field to either type, you **must** add it to both and update the
/// `from_field_copy!` invocation below.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EpisodeDriftCallDto {
    pub activity_anchor: String,
    pub function_name: String,
    pub severity: String,
    pub intent_alignment: f32,
    pub step_alignment: Option<f32>,
    pub cross_encoder_step_score: Option<f32>,
    pub trajectory_drift: Option<f32>,
    pub plan_adherence_score: f32,
    pub citation_mean_similarity: Option<f32>,
    pub citation_coverage: Option<f32>,
}

/// Complete episode snapshot for SSE streaming and one-shot requests.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EpisodeSnapshotDto {
    pub task_id: String,
    pub context_id: String,
    pub agent_id: String,
    pub ref_prefix: String,
    pub status: TerminalStatusDto,
    pub started_timestamp_ms: u64,
    pub duration: EpisodeDurationDto,
    pub token_summary: TokenSummaryDto,
    pub prior_context: Vec<EpisodeEntryDto>,
    pub goal: EpisodeEntryDto,
    pub transcript: Vec<EpisodeEntryDto>,
    pub session_history: Vec<SessionHistoryLineDto>,
    pub intents: Vec<IntentRevisionDto>,
    pub plans: Vec<PlanRevisionDto>,
    pub outcome: EpisodeOutcomeDto,
    /// Task-level drift summary. None when drift scoring was not enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_summary: Option<EpisodeDriftSummaryDto>,
    /// Per-LLM-call drift detail anchored to transcript activity anchors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drift_calls: Vec<EpisodeDriftCallDto>,
}

// ── From impls: provenance domain → API DTO ─────────────────────────────────
//
// Pure field-copy impls (where all field names and types match exactly) are
// generated by the `from_field_copy!` macro below to avoid repetitive
// boilerplate. Only impls with non-trivial transformations are written by hand.

/// Generate a `From<$src> for $dst` impl by moving each listed field directly.
/// All field names must exist on both the source and destination with identical
/// types; the macro produces a compile error otherwise.
macro_rules! from_field_copy {
    ($src:path => $dst:ty { $($field:ident),* $(,)? }) => {
        impl From<$src> for $dst {
            fn from(v: $src) -> Self {
                Self { $($field: v.$field),* }
            }
        }
    };
}

from_field_copy!(baml_rt_provenance::EpisodeDuration => EpisodeDurationDto {
    active_ms, wait_ms, wall_clock_ms
});

from_field_copy!(baml_rt_provenance::TokenSummary => TokenSummaryDto {
    prompt_tokens, completion_tokens, total_tokens, llm_call_count, llm_duration_ms
});

from_field_copy!(baml_rt_provenance::SessionHistoryLine => SessionHistoryLineDto {
    role, content
});

from_field_copy!(baml_rt_provenance::ArtifactSummary => ArtifactSummaryDto {
    name, media_type
});

from_field_copy!(baml_rt_provenance::IntentRevision => IntentRevisionDto {
    intent_id, description, activity_anchor, timestamp_ms,
    superseded_by_next, supersession_from_previous, derived_citation_strings
});

from_field_copy!(baml_rt_provenance::PlanStepEntry => PlanStepEntryDto {
    step_id, description, status, timestamp_ms, citation_strings
});

impl From<baml_rt_provenance::EpisodeDriftSummary> for EpisodeDriftSummaryDto {
    fn from(v: baml_rt_provenance::EpisodeDriftSummary) -> Self {
        Self {
            composite_severity: v.composite_severity.as_str().to_owned(),
            intent_alignment: v.intent_alignment,
            step_alignment: v.step_alignment,
            trajectory_drift: v.trajectory_drift,
            plan_adherence_score: v.plan_adherence_score,
            scored_call_count: v.scored_call_count,
            warn_count: v.warn_count,
            block_count: v.block_count,
        }
    }
}

impl From<baml_rt_provenance::EpisodeDriftCall> for EpisodeDriftCallDto {
    fn from(v: baml_rt_provenance::EpisodeDriftCall) -> Self {
        Self {
            activity_anchor: v.activity_anchor,
            function_name: v.function_name,
            severity: v.severity.as_str().to_owned(),
            intent_alignment: v.intent_alignment,
            step_alignment: v.step_alignment,
            cross_encoder_step_score: v.cross_encoder_step_score,
            trajectory_drift: v.trajectory_drift,
            plan_adherence_score: v.plan_adherence_score,
            citation_mean_similarity: v.citation_mean_similarity,
            citation_coverage: v.citation_coverage,
        }
    }
}

// ── Non-trivial From impls (enum matches, nested conversions) ────────────────

impl From<baml_rt_provenance::TerminalStatus> for TerminalStatusDto {
    fn from(s: baml_rt_provenance::TerminalStatus) -> Self {
        match s {
            baml_rt_provenance::TerminalStatus::Completed => Self::Completed,
            baml_rt_provenance::TerminalStatus::Failed => Self::Failed,
            baml_rt_provenance::TerminalStatus::Canceled => Self::Canceled,
            baml_rt_provenance::TerminalStatus::Rejected => Self::Rejected,
            baml_rt_provenance::TerminalStatus::Other(s) => Self::Other(s),
        }
    }
}

impl From<baml_rt_provenance::StepType> for StepTypeDto {
    fn from(s: baml_rt_provenance::StepType) -> Self {
        match s {
            baml_rt_provenance::StepType::Message => Self::Message,
            baml_rt_provenance::StepType::ToolCall => Self::ToolCall,
            baml_rt_provenance::StepType::ToolRead => Self::ToolRead,
            baml_rt_provenance::StepType::ToolResult => Self::ToolResult,
            baml_rt_provenance::StepType::PlanRevision => Self::PlanRevision,
            baml_rt_provenance::StepType::StatusTransition => Self::StatusTransition,
            baml_rt_provenance::StepType::ArtifactEmitted => Self::ArtifactEmitted,
        }
    }
}

impl From<baml_rt_provenance::EpisodeContent> for EpisodeContentDto {
    fn from(c: baml_rt_provenance::EpisodeContent) -> Self {
        match c {
            baml_rt_provenance::EpisodeContent::Text(text) => Self::Text { text },
            baml_rt_provenance::EpisodeContent::ToolInvocation {
                tool_name,
                description,
            } => Self::ToolInvocation {
                tool_name,
                description,
            },
            baml_rt_provenance::EpisodeContent::ToolOutput {
                tool_name,
                summary,
                line_count,
                byte_count,
                lines,
            } => Self::ToolOutput {
                tool_name,
                summary,
                line_count,
                byte_count,
                lines,
            },
            baml_rt_provenance::EpisodeContent::PlanRevisionRef { summary } => {
                Self::PlanRevisionRef { summary }
            }
            baml_rt_provenance::EpisodeContent::StatusChange { old, new, message } => {
                Self::StatusChange { old, new, message }
            }
            baml_rt_provenance::EpisodeContent::Artifact {
                name,
                media_type,
                size_bytes,
            } => Self::Artifact {
                name,
                media_type,
                size_bytes,
            },
        }
    }
}

impl From<baml_rt_provenance::EpisodeEntry> for EpisodeEntryDto {
    fn from(e: baml_rt_provenance::EpisodeEntry) -> Self {
        Self {
            seq: e.seq,
            step_type: e.step_type.into(),
            role: e.role,
            elapsed_ms: e.elapsed_ms,
            content: e.content.into(),
            activity_anchor: e.activity_anchor,
            citation_strings: e.citation_strings,
        }
    }
}

impl From<baml_rt_provenance::PlanRevision> for PlanRevisionDto {
    fn from(p: baml_rt_provenance::PlanRevision) -> Self {
        Self {
            plan_id: p.plan_id,
            intent_id: p.intent_id,
            activity_anchor: p.activity_anchor,
            timestamp_ms: p.timestamp_ms,
            superseded_by_next: p.superseded_by_next,
            steps: p.steps.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<baml_rt_provenance::EpisodeOutcome> for EpisodeOutcomeDto {
    fn from(o: baml_rt_provenance::EpisodeOutcome) -> Self {
        Self {
            final_message: o.final_message,
            artifacts: o.artifacts.into_iter().map(Into::into).collect(),
            citation_strings: o.citation_strings,
            token_summary: o.token_summary.into(),
            duration: o.duration.into(),
        }
    }
}

impl From<baml_rt_provenance::Episode> for EpisodeSnapshotDto {
    fn from(ep: baml_rt_provenance::Episode) -> Self {
        Self {
            task_id: ep.task_id.as_str().to_string(),
            context_id: ep.context_id.as_str().to_string(),
            agent_id: ep.agent_id.as_str().to_string(),
            ref_prefix: ep.ref_prefix.as_str().to_string(),
            status: ep.status.into(),
            started_timestamp_ms: ep.started_timestamp_ms,
            duration: ep.duration.into(),
            token_summary: ep.token_summary.into(),
            prior_context: ep.prior_context.into_iter().map(Into::into).collect(),
            goal: ep.goal.into(),
            transcript: ep.transcript.into_iter().map(Into::into).collect(),
            session_history: ep.session_history.into_iter().map(Into::into).collect(),
            intents: ep.intents.into_iter().map(Into::into).collect(),
            plans: ep.plans.into_iter().map(Into::into).collect(),
            outcome: ep.outcome.into(),
            drift_summary: ep.drift_summary.map(Into::into),
            drift_calls: ep.drift_calls.into_iter().map(Into::into).collect(),
        }
    }
}
