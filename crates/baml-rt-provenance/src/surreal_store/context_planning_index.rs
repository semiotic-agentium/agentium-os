//! Write-maintained planning index per `(context_id, task_id)`.

use baml_rt_core::ids::{ContextId, TaskId};
use serde_json::Value;

use super::{SurrealProvenanceStore, helpers::map_surreal_error};
use crate::{
    error::Result,
    events::{ProvEvent, ProvEventData},
    metamodel::SemanticEdge,
    normalizer::task_entity_id_string,
    surreal_tables::TBL_CONTEXT_PLANNING_INDEX,
};

#[derive(Debug, Clone, Default)]
pub struct ContextPlanningIndexRow {
    pub context_id: String,
    pub task_id: String,
    pub latest_intent_node_id: Option<String>,
    pub latest_plan_node_id: Option<String>,
    pub intent_count: u32,
    pub plan_count: u32,
    pub latest_planning_event_order: u64,
}

impl SurrealProvenanceStore {
    pub(super) async fn update_context_planning_index(&self, event: &ProvEvent) -> Result<()> {
        let Some(context_id) = event.context_id_opt() else {
            return Ok(());
        };
        let task_id = match event.data() {
            ProvEventData::IntentResolved { task_id, .. }
            | ProvEventData::PlanGenerated { task_id, .. }
            | ProvEventData::PlanStepStatusChanged { task_id, .. } => task_id.clone(),
            _ => return Ok(()),
        };

        let event_order = event.timestamp_ms();
        let mut row = self
            .load_context_planning_row(context_id.as_str(), task_id.as_str())
            .await?
            .unwrap_or(ContextPlanningIndexRow {
                context_id: context_id.as_str().to_string(),
                task_id: task_id.as_str().to_string(),
                ..Default::default()
            });

        match event.data() {
            ProvEventData::IntentResolved { .. } => {
                row.intent_count = row.intent_count.saturating_add(1);
                if let Some(node_id) = self
                    .planning_head_node_id(&task_id, SemanticEdge::WasLastResolvedTo)
                    .await?
                {
                    row.latest_intent_node_id = Some(node_id);
                }
            }
            ProvEventData::PlanGenerated { .. } => {
                row.plan_count = row.plan_count.saturating_add(1);
                if let Some(node_id) = self
                    .planning_head_node_id(&task_id, SemanticEdge::WasLastPlannedTo)
                    .await?
                {
                    row.latest_plan_node_id = Some(node_id);
                }
            }
            ProvEventData::PlanStepStatusChanged { .. } => {}
            _ => return Ok(()),
        }
        row.latest_planning_event_order = row.latest_planning_event_order.max(event_order);
        self.upsert_context_planning_row(&row).await
    }

    pub async fn list_context_planning_index(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ContextPlanningIndexRow>> {
        let sql = if task_id.is_some() {
            format!(
                "SELECT context_id, task_id, latest_intent_node_id, latest_plan_node_id, \
                 intent_count, plan_count, latest_planning_event_order \
                 FROM {TBL_CONTEXT_PLANNING_INDEX} \
                 WHERE context_id = $context_id AND task_id = $task_id \
                 ORDER BY latest_planning_event_order DESC"
            )
        } else {
            format!(
                "SELECT context_id, task_id, latest_intent_node_id, latest_plan_node_id, \
                 intent_count, plan_count, latest_planning_event_order \
                 FROM {TBL_CONTEXT_PLANNING_INDEX} \
                 WHERE context_id = $context_id \
                 ORDER BY latest_planning_event_order DESC"
            )
        };
        let mut query = self
            .db
            .query(&sql)
            .bind(("context_id", context_id.as_str().to_string()));
        if let Some(task_id) = task_id {
            query = query.bind(("task_id", task_id.as_str().to_string()));
        }
        let response = query.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = super::helpers::check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.iter().filter_map(row_from_value).collect())
    }

    async fn load_context_planning_row(
        &self,
        context_id: &str,
        task_id: &str,
    ) -> Result<Option<ContextPlanningIndexRow>> {
        let sql = format!(
            "SELECT context_id, task_id, latest_intent_node_id, latest_plan_node_id, \
             intent_count, plan_count, latest_planning_event_order \
             FROM {TBL_CONTEXT_PLANNING_INDEX} \
             WHERE context_id = $context_id AND task_id = $task_id LIMIT 1"
        );
        let response = self
            .db
            .query(&sql)
            .bind(("context_id", context_id.to_string()))
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = super::helpers::check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.first().and_then(row_from_value))
    }

    async fn upsert_context_planning_row(&self, row: &ContextPlanningIndexRow) -> Result<()> {
        let sql = format!(
            "UPSERT {TBL_CONTEXT_PLANNING_INDEX} SET \
               context_id = $context_id, \
               task_id = $task_id, \
               latest_intent_node_id = $latest_intent_node_id, \
               latest_plan_node_id = $latest_plan_node_id, \
               intent_count = $intent_count, \
               plan_count = $plan_count, \
               latest_planning_event_order = $latest_planning_event_order \
             WHERE context_id = $context_id AND task_id = $task_id"
        );
        self.db
            .query(&sql)
            .bind(("context_id", row.context_id.clone()))
            .bind(("task_id", row.task_id.clone()))
            .bind(("latest_intent_node_id", row.latest_intent_node_id.clone()))
            .bind(("latest_plan_node_id", row.latest_plan_node_id.clone()))
            .bind(("intent_count", row.intent_count))
            .bind(("plan_count", row.plan_count))
            .bind((
                "latest_planning_event_order",
                row.latest_planning_event_order,
            ))
            .await
            .map_err(map_surreal_error)?
            .check()
            .map_err(map_surreal_error)?;
        Ok(())
    }

    pub(super) async fn planning_head_node_id(
        &self,
        task_id: &TaskId,
        edge: SemanticEdge,
    ) -> Result<Option<String>> {
        let task_node_id = task_entity_id_string(task_id);
        let (sql, binds) = crate::metamodel::EdgeProjection::for_edge(edge)
            .from_id_in(&[task_node_id])
            .into_surreal();
        let mut q = self.db.query(sql);
        if let Some(obj) = binds.as_object() {
            for (k, v) in obj {
                q = q.bind((k.clone(), v.clone()));
            }
        }
        let mut response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = response.take(0).map_err(map_surreal_error)?;
        Ok(rows
            .first()
            .and_then(|r| r.get("to_id").and_then(Value::as_str))
            .map(str::to_string))
    }
}

fn row_from_value(row: &Value) -> Option<ContextPlanningIndexRow> {
    Some(ContextPlanningIndexRow {
        context_id: row.get("context_id")?.as_str()?.to_string(),
        task_id: row.get("task_id")?.as_str()?.to_string(),
        latest_intent_node_id: row
            .get("latest_intent_node_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        latest_plan_node_id: row
            .get("latest_plan_node_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        intent_count: row.get("intent_count").and_then(Value::as_u64).unwrap_or(0) as u32,
        plan_count: row.get("plan_count").and_then(Value::as_u64).unwrap_or(0) as u32,
        latest_planning_event_order: row
            .get("latest_planning_event_order")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}
