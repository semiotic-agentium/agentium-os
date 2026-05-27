//! Planning head-pointer and history reads.

use async_trait::async_trait;
use baml_rt_core::ids::TaskId;

use crate::{
    error::Result,
    store::{PlanningIntentRecord, PlanningPlanRecord},
};

#[derive(Debug, Clone)]
pub struct PlanningSliceSpec {
    pub task_id: TaskId,
    pub history_limit: usize,
}

#[async_trait]
pub trait PlanningReader: Send + Sync {
    async fn current_intent(&self, task_id: &TaskId) -> Result<Option<PlanningIntentRecord>>;
    async fn current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>>;
    async fn intent_history(
        &self,
        task_id: &TaskId,
        limit: usize,
    ) -> Result<Vec<PlanningIntentRecord>>;
    async fn plan_history(&self, task_id: &TaskId, limit: usize)
    -> Result<Vec<PlanningPlanRecord>>;
}
