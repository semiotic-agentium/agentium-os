// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Batched context-scoped planning reads — index-authoritative, O(1) round-trips per phase.

use std::collections::{HashMap, HashSet};

use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    context_planning_index::ContextPlanningIndexRow,
    helpers::{check_and_take_zero, map_surreal_error},
    planning_record_parse::{group_intent_rows, plan_record_from_props},
};
use crate::{
    error::{ProvenanceError, Result},
    normalizer::task_entity_id_string,
    store::{PlanningIntentRecord as StoreIntent, PlanningPlanRecord},
    surreal_tables::{TBL_EDGE, TBL_NODE},
    task_graph_reader::TaskGraphReader,
    vocabulary::semantic_labels,
};

/// Scope for batched planning graph reads (store boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningScopeQuery {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub agent_package: Option<String>,
    pub agent_id: Option<baml_rt_core::ids::AgentId>,
    pub history_limit: usize,
}

/// One task's planning projection from a batched read.
#[derive(Debug, Clone)]
pub struct TaskPlanningBatchRow {
    pub task_id: String,
    pub current_intent: Option<StoreIntent>,
    pub current_plan: Option<PlanningPlanRecord>,
    pub intent_history: Vec<StoreIntent>,
    pub plan_history: Vec<PlanningPlanRecord>,
}

impl SurrealProvenanceStore {
    /// Batched planning read for a context scope.
    ///
    /// Reads only from `context_planning_index` (write-maintained). Empty index → empty tasks.
    /// Returns `(all_task_ids, tasks_with_planning_data)`.
    pub async fn query_planning_batch(
        &self,
        scope: &PlanningScopeQuery,
    ) -> Result<(Vec<String>, Vec<TaskPlanningBatchRow>)> {
        let all_task_ids = self
            .list_scoped(&scope.context_id)
            .await?
            .into_iter()
            .map(|r| r.task_id().as_str().to_string())
            .collect::<Vec<_>>();

        let mut index_rows = self
            .list_context_planning_index(&scope.context_id, scope.task_id.as_ref())
            .await?;

        if let Some(ref task_id) = scope.task_id {
            index_rows.retain(|r| r.task_id == task_id.as_str());
        }

        index_rows.retain(|r| r.intent_count > 0 || r.plan_count > 0);
        if index_rows.is_empty() {
            return Ok((all_task_ids, Vec::new()));
        }

        if scope.agent_id.is_some() || scope.agent_package.is_some() {
            index_rows = self
                .filter_planning_index_by_agent(scope, &index_rows)
                .await?;
            if index_rows.is_empty() {
                return Ok((all_task_ids, Vec::new()));
            }
        }

        let history_limit = scope.history_limit.max(1);
        let task_ids: Vec<TaskId> = index_rows
            .iter()
            .map(|r| TaskId::from_external(ExternalId::new(r.task_id.clone())))
            .collect();

        let intent_histories = self
            .batch_intent_histories(&task_ids, history_limit)
            .await?;
        let plan_histories = self.batch_plan_histories(&task_ids, history_limit).await?;

        let mut tasks = Vec::with_capacity(index_rows.len());
        for row in index_rows {
            let task_id = TaskId::from_external(ExternalId::new(row.task_id.clone()));
            let current_intent = self.resolve_index_intent(&row, &task_id).await?;
            let current_plan = self.resolve_index_plan(&row, &task_id).await?;
            tasks.push(TaskPlanningBatchRow {
                task_id: row.task_id,
                current_intent,
                current_plan,
                intent_history: intent_histories
                    .get(task_id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                plan_history: plan_histories
                    .get(task_id.as_str())
                    .cloned()
                    .unwrap_or_default(),
            });
        }

        Ok((all_task_ids, tasks))
    }

    async fn resolve_index_intent(
        &self,
        row: &ContextPlanningIndexRow,
        task_id: &TaskId,
    ) -> Result<Option<StoreIntent>> {
        match row.latest_intent_node_id.as_deref() {
            Some(node_id) => self
                .hydrate_intent_node(node_id)
                .await?
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: node_id.to_string(),
                    reason: format!(
                        "context_planning_index latest_intent_node_id for task {} points to missing or invalid node",
                        task_id.as_str()
                    ),
                })
                .map(Some),
            None if row.intent_count > 0 => Err(ProvenanceError::InvalidEvent {
                activity_anchor: task_id.as_str().to_string(),
                reason: format!(
                    "context_planning_index intent_count={} but latest_intent_node_id is absent for task {}",
                    row.intent_count, task_id.as_str()
                ),
            }),
            None => Ok(None),
        }
    }

    async fn resolve_index_plan(
        &self,
        row: &ContextPlanningIndexRow,
        task_id: &TaskId,
    ) -> Result<Option<PlanningPlanRecord>> {
        match row.latest_plan_node_id.as_deref() {
            Some(node_id) => self
                .hydrate_plan_node(task_id, node_id)
                .await?
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: node_id.to_string(),
                    reason: format!(
                        "context_planning_index latest_plan_node_id for task {} points to missing or invalid node",
                        task_id.as_str()
                    ),
                })
                .map(Some),
            None if row.plan_count > 0 => Err(ProvenanceError::InvalidEvent {
                activity_anchor: task_id.as_str().to_string(),
                reason: format!(
                    "context_planning_index plan_count={} but latest_plan_node_id is absent for task {}",
                    row.plan_count, task_id.as_str()
                ),
            }),
            None => Ok(None),
        }
    }

    async fn filter_planning_index_by_agent(
        &self,
        scope: &PlanningScopeQuery,
        rows: &[ContextPlanningIndexRow],
    ) -> Result<Vec<ContextPlanningIndexRow>> {
        let agent_runtime_index = self.load_agent_runtime_index().await?;
        let mut out = Vec::new();
        for row in rows {
            let task_id = TaskId::from_external(ExternalId::new(row.task_id.clone()));
            let resolution = self.get_task_agent_id(&task_id).await?;
            let Some(agent_id) = resolution.for_normalization() else {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: task_id.as_str().to_string(),
                    reason: "agent filter active but task has no resolvable agent_id".into(),
                });
            };
            if let Some(ref filter_id) = scope.agent_id
                && agent_id != *filter_id
            {
                continue;
            }
            if let Some(ref package) = scope.agent_package {
                let Some((agent_package, _version)) = agent_runtime_index
                    .identity_by_agent_id
                    .get(agent_id.as_str())
                else {
                    return Err(ProvenanceError::InvalidEvent {
                        activity_anchor: agent_id.as_str().to_string(),
                        reason: format!(
                            "agent filter package={package} but agent_id has no runtime index entry"
                        ),
                    });
                };
                if agent_package != package {
                    continue;
                }
            }
            out.push(row.clone());
        }
        Ok(out)
    }

    async fn batch_intent_histories(
        &self,
        task_ids: &[TaskId],
        limit: usize,
    ) -> Result<HashMap<String, Vec<StoreIntent>>> {
        let task_node_ids: Vec<String> = task_ids.iter().map(task_entity_id_string).collect();
        if task_node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let query = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id IN $task_node_ids AND rel_type = '{has_intent}'\
             ) ORDER BY event_order DESC",
            has_intent = semantic_labels::HAS_INTENT,
        );
        let response = self
            .db
            .query(&query)
            .bind(("task_node_ids", task_node_ids))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(group_intent_rows(&rows, limit))
    }

    async fn batch_plan_histories(
        &self,
        task_ids: &[TaskId],
        limit: usize,
    ) -> Result<HashMap<String, Vec<PlanningPlanRecord>>> {
        let task_node_ids: Vec<String> = task_ids.iter().map(task_entity_id_string).collect();
        if task_node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let query = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id IN $task_node_ids AND rel_type = '{has_plan}'\
             ) ORDER BY event_order DESC",
            has_plan = semantic_labels::HAS_PLAN,
        );
        let response = self
            .db
            .query(&query)
            .bind(("task_node_ids", task_node_ids))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

        let mut plan_ids_by_task: HashMap<String, Vec<String>> = HashMap::new();
        let mut row_props: Vec<(String, Value)> = Vec::new();
        for row in &rows {
            let Some(props) = row.get("props") else {
                continue;
            };
            let Some(task_id_value) = props.get("a2a_task_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(plan_id) = props.get("a2a_plan_id").and_then(Value::as_str) else {
                continue;
            };
            plan_ids_by_task
                .entry(task_id_value.to_string())
                .or_default()
                .push(plan_id.to_string());
            row_props.push((task_id_value.to_string(), props.clone()));
        }

        let mut steps_by_task_plan: HashMap<(String, String), Vec<_>> = HashMap::new();
        for task_id in task_ids {
            let tid = task_id.as_str();
            if let Some(plan_ids) = plan_ids_by_task.get(tid) {
                let unique: Vec<String> = plan_ids
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let steps = self.query_plan_steps_for_plans(task_id, &unique).await?;
                for (plan_id, step_records) in steps {
                    steps_by_task_plan.insert((tid.to_string(), plan_id), step_records);
                }
            }
        }

        let mut grouped: HashMap<String, Vec<PlanningPlanRecord>> = HashMap::new();
        for (task_id_value, props) in row_props {
            let entry = grouped.entry(task_id_value.clone()).or_default();
            if entry.len() >= limit {
                continue;
            }
            if let Some(plan) = plan_record_from_props(&task_id_value, &props, &steps_by_task_plan)
            {
                entry.push(plan);
            }
        }
        Ok(grouped)
    }
}
