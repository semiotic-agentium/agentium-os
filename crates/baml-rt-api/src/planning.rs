//! Narrow trait for context-scoped planning state serving.
//! Implemented by the runtime when SurrealDB provenance is enabled.

use std::{error::Error, fmt};

use baml_rt_provenance::{PlanningIntentRecord, PlanningPlanRecord};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum PlanningError {
    NotFound,
    Unavailable,
    Other(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for PlanningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanningError::NotFound => write!(f, "no planning data found for the given context"),
            PlanningError::Unavailable => write!(f, "planning service unavailable"),
            PlanningError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl Error for PlanningError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningStepSummary {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub in_progress: usize,
    pub pending: usize,
}

/// One citation as surfaced in the planning API — ref string, resolved preview, and similarity.
///
/// This is the API-layer projection of `LlmCitationSimilarity` from the provenance
/// record. The `content_preview` field carries **resolved text** for the cited ref
/// (history or archive), stored at scoring time so consumers need not re-resolve
/// ephemeral `RefTable` indices.
///
/// ## Interpreting `similarity`
///
/// - `≥ 0.65` — strong grounding: response closely paraphrases this cited entry
/// - `0.40–0.65` — moderate: same domain, different specifics
/// - `< 0.40` — likely wrong archive cited, or unrelated evidence
/// - `negated = true` — counter-evidence: model explicitly rejected this entry;
///   `similarity` is still meaningful (shows how closely the rejection is worded)
///   but this citation does NOT contribute to `mean_similarity` in drift scoring
///
/// ## Citation quality as a BIPIA indicator
///
/// When `negated = false` and the mean `similarity` across all citations for a
/// call is `> 0.85`, combined with low `step_alignment` (`< 0.62` for synthesis
/// steps, `< 0.45` for specific steps), the call is flagged by the 2D BIPIA rule.
/// See `baml_rt_embedding::score_bipia_signal` for the composite firewall.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationDetail {
    /// Exact string the LLM emitted, e.g. `"#1"`, `"@2:3-5"`, `"!@1"`.
    /// The leading `!` indicates counter-evidence.
    pub raw: String,
    /// The ref number `N`.
    pub n: u32,
    /// `true` = history ref (`#N`): a session history line (user/assistant/tool-call).
    /// `false` = archive ref (`@N`): an archived tool result blob.
    pub is_history: bool,
    /// `true` = counter-evidence (`!` prefix): the LLM explicitly rejected this entry.
    /// Excluded from drift `mean_similarity` but reported here for auditability.
    pub negated: bool,
    /// Cosine similarity between the LLM's decision text and this citation's content.
    pub similarity: f32,
    /// Stable provenance event id. Use for cross-referencing in the provenance graph
    /// (`/contexts/{id}/mermaid`, `/provenance/llm-calls`, etc.).
    pub activity_anchor: String,
    /// Resolved content for the `#N` or `@N` ref — what the model *claimed* it grounded in.
    pub content_preview: String,
}

/// Detail for a single LLM call that triggered a warn or block drift severity.
///
/// Provides plan-anchored drift signals (intent, step, cross-encoder) and the
/// **checked citation** trail (`raw` ref strings + resolved previews), not opaque
/// evidence prose alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftedCallDetail {
    pub function_name: String,
    pub severity: String,
    pub intent_alignment: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_alignment: Option<f32>,
    /// Cross-encoder logit score (JINA reranker) for the step–response pair.
    /// Always present when `step_alignment` is present. Catches injections that
    /// cosine similarity misses (e.g. same vocabulary, wrong action).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_encoder_step_score: Option<f32>,
    pub intent_text_preview: String,
    pub response_text_preview: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub step_text_preview: String,
    /// Per-citation resolution and similarity for this call (see [`CitationDetail`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationDetail>,
}

/// Summary of plan-anchored drift state for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPlanDriftSummary {
    /// Latest composite severity across all strategic dimensions.
    pub composite_severity: Option<String>,
    /// Latest intent alignment score.
    pub intent_alignment: Option<f32>,
    /// Latest step alignment score.
    pub step_alignment: Option<f32>,
    /// Latest trajectory drift score.
    pub trajectory_drift: Option<f32>,
    /// Latest plan adherence score.
    pub plan_adherence_score: Option<f32>,
    /// Number of LLM calls that had plan drift scored.
    pub scored_call_count: u32,
    /// Number of calls with warn severity.
    pub warn_count: u32,
    /// Number of calls with block severity.
    pub block_count: u32,
    /// Individual calls that triggered warn or block, with textual evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drifted_calls: Vec<DriftedCallDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPlanningSnapshot {
    pub task_id: String,
    pub current_intent: Option<PlanningIntentRecord>,
    pub current_plan: Option<PlanningPlanRecord>,
    pub intent_history: Vec<PlanningIntentRecord>,
    pub plan_history: Vec<PlanningPlanRecord>,
    pub step_summary: PlanningStepSummary,
    /// Plan-anchored drift summary.  `None` when no plan drift data exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<TaskPlanDriftSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPlanningResponse {
    pub context_id: String,
    pub tasks: Vec<TaskPlanningSnapshot>,
}

#[async_trait::async_trait]
pub trait PlanningService: Send + Sync {
    async fn planning_for_context(
        &self,
        context_id: &str,
    ) -> Result<ContextPlanningResponse, PlanningError>;
}
