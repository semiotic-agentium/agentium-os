// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Historic **episode** view: task-scoped transcript and metadata for grep-friendly replay.

use baml_rt_core::ids::{AgentId, ContextId, TaskId};
use baml_rt_embedding::DriftSeverity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::operational::OperationalEventContent;

/// First four hex digits of `sha256(task_id)` — episode ref namespace (e.g. `e5a3`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpisodeRefPrefix(String);

impl EpisodeRefPrefix {
    /// Deterministic 4-character lowercase hex prefix from the task id.
    pub fn from_task_id(task_id: &TaskId) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(task_id.as_str().as_bytes());
        let digest = hasher.finalize();
        let hex = format!("{digest:x}");
        Self(hex.chars().take(4).collect())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Episode-local history ref: `prefix#seq`.
    pub fn format_history(&self, seq: u32) -> String {
        format!("{}#{seq}", self.0)
    }

    /// Episode-local archive ref: `prefix@seq`.
    pub fn format_archive(&self, seq: u32) -> String {
        format!("{}@{seq}", self.0)
    }

    /// Format `prefix@seq:line` or `prefix@seq:start-end` when `range` is `Some((a,b))`.
    pub fn format_archive_lines(&self, seq: u32, range: Option<(usize, usize)>) -> String {
        match range {
            None => self.format_archive(seq),
            Some((a, b)) if a == b => format!("{}@{seq}:{a}", self.0),
            Some((a, b)) => format!("{}@{seq}:{a}-{b}", self.0),
        }
    }
}

/// Terminal task outcome for a historic episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalStatus {
    Completed,
    Failed,
    Canceled,
    Rejected,
    Other(String),
}

impl TerminalStatus {
    /// True when the status represents a completed task lifecycle (not in-progress).
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodeDuration {
    pub active_ms: u64,
    pub wait_ms: u64,
    pub wall_clock_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenSummary {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub llm_call_count: u32,
    pub llm_duration_ms: u64,
}

/// One line of BAML-style `conversation_history`, aligned with the JSON rows from
/// [`baml_rt_tools::prompt_projection::project_prompt_context`]: `role`, `content`, and optional
/// `citations` (wire refs) on message-sourced rows; `#N`/`@N` are episode-prefixed for eval/replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHistoryLine {
    pub role: String,
    pub content: String,
    /// Ref-table strings from graph/CITED for agent messages only; empty when inapplicable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub task_id: TaskId,
    pub context_id: ContextId,
    pub agent_id: AgentId,
    pub ref_prefix: EpisodeRefPrefix,
    pub status: TerminalStatus,
    pub started_timestamp_ms: u64,
    pub duration: EpisodeDuration,
    pub token_summary: TokenSummary,
    pub prior_context: Vec<EpisodeEntry>,
    pub goal: EpisodeEntry,
    pub transcript: Vec<EpisodeEntry>,
    /// Session-style projection of the **merged** episode timeline (prior + in-task conversation +
    /// status + artifacts, see provenance `EpisodeReader`), aligned with live
    /// Projected history rows (same lineage as `conversation_transcript`) and optional per-message `citations`.
    pub session_history: Vec<SessionHistoryLine>,
    pub intents: Vec<IntentRevision>,
    pub plans: Vec<PlanRevision>,
    pub outcome: EpisodeOutcome,
    /// Task-level drift summary. None when drift scoring was not enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_summary: Option<EpisodeDriftSummary>,
    /// Per-LLM-call drift detail anchored to transcript activity anchors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drift_calls: Vec<EpisodeDriftCall>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepType {
    Message,
    ToolCall,
    /// Tool session `Read` op (archive grep / slice) — distinct from a plain tool call.
    ToolRead,
    ToolResult,
    PlanRevision,
    StatusTransition,
    ArtifactEmitted,
    /// Host/system operational row (dispatch failures, poll records, LLM errors).
    OperationalEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeEntry {
    /// Monotonic episode ref index (shared counter for `#` and `@`).
    pub seq: u32,
    pub step_type: StepType,
    pub role: String,
    /// Milliseconds relative to task start (negative for prior-context rows).
    pub elapsed_ms: i64,
    pub content: EpisodeContent,
    pub activity_anchor: String,
    /// Episode-prefixed citation strings from the LLM call that produced this entry.
    /// Populated for agent message entries; empty for all other entry types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citation_strings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EpisodeContent {
    Text(String),
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
    /// Host/system operational provenance (dispatch failures, poll records, LLM errors).
    Operational(OperationalEventContent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentRevision {
    pub intent_id: String,
    pub description: String,
    pub activity_anchor: String,
    pub timestamp_ms: u64,
    pub superseded_by_next: bool,
    pub supersession_from_previous: Option<String>,
    /// Raw citation strings from the graph (`#N`, `@N`, …) at intent resolution time.
    pub derived_citation_strings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRevision {
    pub plan_id: String,
    pub intent_id: String,
    pub activity_anchor: String,
    pub timestamp_ms: u64,
    pub superseded_by_next: bool,
    pub steps: Vec<PlanStepEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepEntry {
    pub step_id: String,
    pub description: String,
    pub status: String,
    pub timestamp_ms: Option<u64>,
    pub citation_strings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeOutcome {
    pub final_message: Option<String>,
    pub artifacts: Vec<ArtifactSummary>,
    pub citation_strings: Vec<String>,
    pub token_summary: TokenSummary,
    pub duration: EpisodeDuration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub name: String,
    pub media_type: Option<String>,
}

/// Task-level drift summary for episodic evaluation and memory formation.
/// Flattened version of the API's TaskPlanDriftSummary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeDriftSummary {
    pub composite_severity: DriftSeverity,
    pub intent_alignment: f32,
    pub step_alignment: Option<f32>,
    pub trajectory_drift: Option<f32>,
    pub plan_adherence_score: f32,
    pub scored_call_count: u32,
    pub warn_count: u32,
    pub block_count: u32,
}

/// Per-LLM-call drift detail anchored to the transcript for episodic evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeDriftCall {
    /// Correlates with EpisodeEntry.activity_anchor in the transcript.
    pub activity_anchor: String,
    pub function_name: String,
    pub severity: DriftSeverity,
    pub intent_alignment: f32,
    pub step_alignment: Option<f32>,
    pub cross_encoder_step_score: Option<f32>,
    pub trajectory_drift: Option<f32>,
    pub plan_adherence_score: f32,
    pub citation_mean_similarity: Option<f32>,
    pub citation_coverage: Option<f32>,
    /// Raw citation strings from the LLM response (e.g. `["#16", "@8"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citation_strings: Vec<String>,
}
