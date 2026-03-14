//! Narrow trait for context-scoped planning state serving.
//! Implemented by the runtime when GraphQLite provenance is enabled.

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPlanningSnapshot {
    pub task_id: String,
    pub current_intent: Option<PlanningIntentRecord>,
    pub current_plan: Option<PlanningPlanRecord>,
    pub intent_history: Vec<PlanningIntentRecord>,
    pub plan_history: Vec<PlanningPlanRecord>,
    pub step_summary: PlanningStepSummary,
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
