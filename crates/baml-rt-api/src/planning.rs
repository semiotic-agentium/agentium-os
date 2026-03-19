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

/// Detail for a single LLM call that triggered a warn or block severity.
/// Provides the textual evidence needed to understand what drifted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftedCallDetail {
    pub function_name: String,
    pub severity: String,
    pub intent_alignment: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_alignment: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_encoder_step_score: Option<f32>,
    pub intent_text_preview: String,
    pub response_text_preview: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub step_text_preview: String,
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
