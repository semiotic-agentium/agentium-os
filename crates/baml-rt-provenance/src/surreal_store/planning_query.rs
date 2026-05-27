// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! [`ProvenancePlanningQuery`] and planning graph helpers (intents, plans, step gate).

use std::collections::HashMap;

use async_trait::async_trait;
use baml_rt_core::{
    bus::PlanningSupersessionKind,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_vocabulary::vocabulary::a2a;
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{
        check_and_take_zero, decode_depends_on, is_step_completed_status, map_surreal_error,
    },
};
use crate::{
    error::{ProvenanceError, Result},
    events::ProvEventData,
    id_semantics::context_entity_id_string,
    metamodel::{EdgeProjection, SemanticEdge},
    normalizer::{plan_entity_id_string, task_entity_id_string},
    read::PlanningReader,
    store::{
        PlanningIntentRecord, PlanningPlanRecord, PlanningPlanStepRecord, ProvenancePlanningQuery,
    },
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{context_scope, semantic_labels},
};

fn supersession_kind_from_prop(props: &Value, key: &str) -> Option<PlanningSupersessionKind> {
    props
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| match s {
            "replaced_by" => Some(PlanningSupersessionKind::ReplacedBy),
            "refined_by" => Some(PlanningSupersessionKind::RefinedBy),
            _ => None,
        })
}

#[async_trait]
impl PlanningReader for SurrealProvenanceStore {
    async fn current_intent(&self, task_id: &TaskId) -> Result<Option<PlanningIntentRecord>> {
        ProvenancePlanningQuery::query_current_intent(self, task_id).await
    }

    async fn current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>> {
        ProvenancePlanningQuery::query_current_plan(self, task_id).await
    }

    async fn intent_history(
        &self,
        task_id: &TaskId,
        limit: usize,
    ) -> Result<Vec<PlanningIntentRecord>> {
        ProvenancePlanningQuery::query_intent_history(self, task_id, Some(limit)).await
    }

    async fn plan_history(
        &self,
        task_id: &TaskId,
        limit: usize,
    ) -> Result<Vec<PlanningPlanRecord>> {
        ProvenancePlanningQuery::query_plan_history(self, task_id, Some(limit)).await
    }
}

#[async_trait]
impl ProvenancePlanningQuery for SurrealProvenanceStore {
    async fn query_current_intent(&self, task_id: &TaskId) -> Result<Option<PlanningIntentRecord>> {
        let node_id = self
            .planning_head_node_id(task_id, SemanticEdge::WasLastResolvedTo)
            .await?;
        let Some(node_id) = node_id else {
            return Ok(None);
        };
        self.hydrate_intent_node(&node_id).await
    }

    async fn query_current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>> {
        let node_id = self
            .planning_head_node_id(task_id, SemanticEdge::WasLastPlannedTo)
            .await?;
        let Some(node_id) = node_id else {
            return Ok(None);
        };
        self.hydrate_plan_node(task_id, &node_id).await
    }

    async fn query_intent_history(
        &self,
        task_id: &TaskId,
        limit: Option<usize>,
    ) -> Result<Vec<PlanningIntentRecord>> {
        let limit_val = limit.unwrap_or(100).max(1);
        let task_node_id = task_entity_id_string(task_id);
        let query = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id = $task_node_id AND rel_type = '{has_intent}'\
             ) ORDER BY event_order DESC LIMIT $limit",
            has_intent = semantic_labels::HAS_INTENT,
        );
        let response = self
            .db
            .query(&query)
            .bind(("task_node_id", task_node_id))
            .bind(("limit", limit_val))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

        let mut intents = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let context_id = props.get("a2a_context_id").and_then(Value::as_str);
            let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
            let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str);
            let intent_id = props.get("a2a_intent_id").and_then(Value::as_str);
            let description = props
                .get("prov_label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (Some(context_id), Some(task_id_value), Some(event_id), Some(intent_id)) =
                (context_id, task_id_value, event_id, intent_id)
            else {
                continue;
            };
            let event_order = props
                .get("a2a_event_order")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            intents.push(PlanningIntentRecord {
                context_id: ContextId::from(context_id),
                task_id: TaskId::from_external(ExternalId::new(task_id_value)),
                activity_anchor_id: ActivityAnchorId::from(event_id),
                intent_id: intent_id.to_string(),
                description: description.to_string(),
                event_order,
                supersession_from_previous: supersession_kind_from_prop(
                    props,
                    a2a::SUPERSESSION_FROM_PREVIOUS,
                ),
                superseded_by_next: supersession_kind_from_prop(props, a2a::SUPERSEDED_BY_NEXT),
            });
        }
        Ok(intents)
    }

    async fn query_plan_history(
        &self,
        task_id: &TaskId,
        limit: Option<usize>,
    ) -> Result<Vec<PlanningPlanRecord>> {
        let limit_val = limit.unwrap_or(100).max(1);
        let task_node_id = task_entity_id_string(task_id);
        let query = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id = $task_node_id AND rel_type = '{has_plan}'\
             ) ORDER BY event_order DESC LIMIT $limit",
            has_plan = semantic_labels::HAS_PLAN,
        );
        let response = self
            .db
            .query(&query)
            .bind(("task_node_id", task_node_id))
            .bind(("limit", limit_val))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

        let plan_ids: Vec<String> = rows
            .iter()
            .filter_map(|row| {
                row.get("props")
                    .and_then(|p| p.get("a2a_plan_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let steps_by_plan = self.query_plan_steps_for_plans(task_id, &plan_ids).await?;

        let mut plans = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let context_id = props.get("a2a_context_id").and_then(Value::as_str);
            let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
            let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str);
            let intent_id = props.get("a2a_intent_id").and_then(Value::as_str);
            let plan_id = props.get("a2a_plan_id").and_then(Value::as_str);
            let (
                Some(context_id),
                Some(task_id_value),
                Some(event_id),
                Some(intent_id),
                Some(plan_id),
            ) = (context_id, task_id_value, event_id, intent_id, plan_id)
            else {
                continue;
            };
            let steps = steps_by_plan.get(plan_id).cloned().unwrap_or_default();
            let event_order = props
                .get("a2a_event_order")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            plans.push(PlanningPlanRecord {
                context_id: ContextId::from(context_id),
                task_id: TaskId::from_external(ExternalId::new(task_id_value)),
                activity_anchor_id: ActivityAnchorId::from(event_id),
                intent_id: intent_id.to_string(),
                plan_id: plan_id.to_string(),
                steps,
                event_order,
                supersession_from_previous: supersession_kind_from_prop(
                    props,
                    a2a::SUPERSESSION_FROM_PREVIOUS,
                ),
                superseded_by_next: supersession_kind_from_prop(props, a2a::SUPERSEDED_BY_NEXT),
            });
        }
        Ok(plans)
    }
}

impl SurrealProvenanceStore {
    async fn planning_head_node_id(
        &self,
        task_id: &TaskId,
        edge: SemanticEdge,
    ) -> Result<Option<String>> {
        let task_node_id = task_entity_id_string(task_id);
        let (sql, binds) = EdgeProjection::for_edge(edge)
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

    async fn hydrate_intent_node(&self, node_id: &str) -> Result<Option<PlanningIntentRecord>> {
        let query = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id = $node_id LIMIT 1"
        );
        let response = self
            .db
            .query(&query)
            .bind(("node_id", node_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let props = row
            .get("props")
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: String::new(),
                reason: format!("intent node missing props: {node_id}"),
            })?;
        let context_id = props.get("a2a_context_id").and_then(Value::as_str);
        let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
        let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str);
        let intent_id = props.get("a2a_intent_id").and_then(Value::as_str);
        let description = props
            .get("prov_label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (Some(context_id), Some(task_id_value), Some(event_id), Some(intent_id)) =
            (context_id, task_id_value, event_id, intent_id)
        else {
            return Ok(None);
        };
        let event_order = props
            .get("a2a_event_order")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok(Some(PlanningIntentRecord {
            context_id: ContextId::from(context_id),
            task_id: TaskId::from_external(ExternalId::new(task_id_value)),
            activity_anchor_id: ActivityAnchorId::from(event_id),
            intent_id: intent_id.to_string(),
            description: description.to_string(),
            event_order,
            supersession_from_previous: supersession_kind_from_prop(
                props,
                a2a::SUPERSESSION_FROM_PREVIOUS,
            ),
            superseded_by_next: supersession_kind_from_prop(props, a2a::SUPERSEDED_BY_NEXT),
        }))
    }

    async fn hydrate_plan_node(
        &self,
        task_id: &TaskId,
        node_id: &str,
    ) -> Result<Option<PlanningPlanRecord>> {
        let query = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id = $node_id LIMIT 1"
        );
        let response = self
            .db
            .query(&query)
            .bind(("node_id", node_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let props = row
            .get("props")
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: String::new(),
                reason: format!("plan node missing props: {node_id}"),
            })?;
        let context_id = props.get("a2a_context_id").and_then(Value::as_str);
        let task_id_value = props.get("a2a_task_id").and_then(Value::as_str);
        let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str);
        let intent_id = props.get("a2a_intent_id").and_then(Value::as_str);
        let plan_id = props.get("a2a_plan_id").and_then(Value::as_str);
        let (Some(context_id), Some(task_id_value), Some(event_id), Some(intent_id), Some(plan_id)) =
            (context_id, task_id_value, event_id, intent_id, plan_id)
        else {
            return Ok(None);
        };
        let steps = self
            .query_plan_steps_for_plans(task_id, &[plan_id.to_string()])
            .await?
            .remove(plan_id)
            .unwrap_or_default();
        let event_order = props
            .get("a2a_event_order")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok(Some(PlanningPlanRecord {
            context_id: ContextId::from(context_id),
            task_id: TaskId::from_external(ExternalId::new(task_id_value)),
            activity_anchor_id: ActivityAnchorId::from(event_id),
            intent_id: intent_id.to_string(),
            plan_id: plan_id.to_string(),
            steps,
            event_order,
            supersession_from_previous: supersession_kind_from_prop(
                props,
                a2a::SUPERSESSION_FROM_PREVIOUS,
            ),
            superseded_by_next: supersession_kind_from_prop(props, a2a::SUPERSEDED_BY_NEXT),
        }))
    }

    // -----------------------------------------------------------------------
    // Graph traversal helpers
    // -----------------------------------------------------------------------

    const TASK_AGENT_ID_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// Single-hop graph traversal via the `WAS_LAST_EXECUTED_BY`
    /// head-pointer edge introduced by the relational-shadow excision.
    ///
    /// The previous implementation walked
    /// `Task -[WAS_CREATED_BY]-> TaskExecution -[WAS_EXECUTED_BY]-> AgentRuntimeInstance`
    /// and required a process-local cache to amortise three round-trips
    /// against the hot `message.sendStream` path. The normalizer now
    /// re-points `WAS_LAST_EXECUTED_BY` from the `Task` to the current
    /// `AgentRuntimeInstance` atomically inside the same write batch, so
    /// resolution collapses to a single edge lookup plus one node read
    /// for the `a2a_agent_id` property — and the cache layer is gone.
    pub(crate) async fn get_task_agent_id(
        &self,
        task_id: &TaskId,
    ) -> Result<crate::store::TaskAgentResolution> {
        match tokio::time::timeout(
            Self::TASK_AGENT_ID_TIMEOUT,
            self.get_task_agent_id_inner(task_id),
        )
        .await
        {
            Ok(res) => res,
            Err(_elapsed) => {
                tracing::warn!(
                    task_id = task_id.as_str(),
                    timeout_secs = Self::TASK_AGENT_ID_TIMEOUT.as_secs(),
                    "get_task_agent_id timed out — agent-scoped normalization skipped for this event"
                );
                Ok(crate::store::TaskAgentResolution::TimedOut)
            }
        }
    }

    async fn get_task_agent_id_inner(
        &self,
        task_id: &TaskId,
    ) -> Result<crate::store::TaskAgentResolution> {
        use crate::store::TaskAgentResolution;
        let task_entity_id = task_entity_id_string(task_id);
        let hop1 = format!(
            "SELECT VALUE to_id FROM {TBL_EDGE} \
             WHERE from_id = $task_node AND rel_type = $rel_last LIMIT 1"
        );
        let r1 = self
            .db
            .query(&hop1)
            .bind(("task_node", task_entity_id))
            .bind(("rel_last", semantic_labels::WAS_LAST_EXECUTED_BY))
            .await
            .map_err(map_surreal_error)?;
        let agent_node_ids: Vec<Value> = check_and_take_zero(r1, map_surreal_error)?;
        let Some(agent_node_id) = agent_node_ids.first().and_then(Value::as_str) else {
            return Ok(TaskAgentResolution::NotLinked);
        };

        let hop2 = format!(
            "SELECT props.a2a_agent_id AS agent_id OMIT id FROM {TBL_NODE} \
             WHERE node_id = $agent_node LIMIT 1"
        );
        let r2 = self
            .db
            .query(&hop2)
            .bind(("agent_node", agent_node_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(r2, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(TaskAgentResolution::NotLinked);
        };
        let agent_id_str = row.get("agent_id").and_then(Value::as_str);
        let Some(agent_id_str) = agent_id_str else {
            return Ok(TaskAgentResolution::NotLinked);
        };
        if agent_id_str.trim().is_empty() {
            return Ok(TaskAgentResolution::NotLinked);
        }
        UuidId::parse_str(agent_id_str)
            .map(AgentId::from_uuid)
            .map(TaskAgentResolution::Resolved)
            .map_err(|e| ProvenanceError::InvalidEvent {
                activity_anchor: String::new(),
                reason: format!("task agent instance id invalid UUID: {agent_id_str:?}: {e}"),
            })
    }

    pub(crate) async fn enforce_step_completion_gate(
        &self,
        event: &crate::events::ProvEvent,
    ) -> Result<()> {
        let ProvEventData::PlanStepStatusChanged {
            task_id,
            plan_id,
            step_id,
            new_status,
            ..
        } = event.data()
        else {
            return Ok(());
        };
        if !is_step_completed_status(new_status) {
            return Ok(());
        }
        let context_id = event.context_id().as_str().to_string();
        let deps = self
            .fetch_step_dependencies(task_id.as_str(), plan_id.as_str(), step_id.as_str())
            .await?;
        for dep in deps {
            let completed = self
                .is_step_completed(task_id.as_str(), plan_id.as_str(), &dep)
                .await?;
            if !completed {
                return Err(ProvenanceError::InvalidEvent {
                    activity_anchor: event.id().as_str().to_string(),
                    reason: format!(
                        "step completion rejected: dependency step not completed (plan_id={plan_id}, step_id={step_id}, depends_on={dep})"
                    ),
                });
            }
        }
        let has_evidence = self
            .has_terminal_step_evidence(
                &context_id,
                task_id.as_str(),
                plan_id.as_str(),
                step_id.as_str(),
            )
            .await?;
        if !has_evidence {
            return Err(ProvenanceError::InvalidEvent {
                activity_anchor: event.id().as_str().to_string(),
                reason: format!(
                    "step completion rejected: no terminal LLM/tool evidence linked to step (plan_id={plan_id}, step_id={step_id})"
                ),
            });
        }
        Ok(())
    }

    async fn fetch_step_dependencies(
        &self,
        task_id: &str,
        plan_id: &str,
        step_id: &str,
    ) -> Result<Vec<String>> {
        let step_node_id =
            crate::id_semantics::plan_step_entity_id_string(task_id, plan_id, step_id);
        let query = format!(
            "SELECT props.a2a_depends_on AS deps FROM {TBL_NODE} \
             WHERE node_id = $step_node_id LIMIT 1"
        );
        let response = self
            .db
            .query(&query)
            .bind(("step_node_id", step_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(Vec::new());
        };
        let deps_raw = row.get("deps").and_then(Value::as_str).map(String::from);
        Ok(decode_depends_on(deps_raw))
    }

    async fn is_step_completed(&self, task_id: &str, plan_id: &str, step_id: &str) -> Result<bool> {
        let step_node_id =
            crate::id_semantics::plan_step_entity_id_string(task_id, plan_id, step_id);
        let query = format!(
            "SELECT props.a2a_status AS status FROM {TBL_NODE} \
             WHERE node_id = $step_node_id LIMIT 1"
        );
        let response = self
            .db
            .query(&query)
            .bind(("step_node_id", step_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(false);
        };
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(is_step_completed_status(status))
    }

    async fn has_terminal_step_evidence(
        &self,
        context_id: &str,
        task_id: &str,
        plan_id: &str,
        step_id: &str,
    ) -> Result<bool> {
        let ctx_node_id = context_entity_id_string(context_id);
        let task_exec_node = crate::id_semantics::task_execution_activity_id_string(task_id);
        let scoped = context_scope::SCOPED_TO;
        let task_call = crate::vocabulary::a2a_relations::TASK_CALL;
        for label in ["LlmCall", "ToolCall"] {
            let query = format!(
                "SELECT node_id FROM {TBL_NODE} \
                 WHERE label = '{label}' \
                   AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
                     WHERE to_id = $ctx_node AND rel_type = '{scoped}') \
                   AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
                     WHERE from_id = $task_exec_node AND rel_type = '{task_call}') \
                   AND props.a2a_plan_id = $plan_id \
                   AND props.a2a_step_id = $step_id \
                   AND props.a2a_activity_outcome = 'Success' \
                 LIMIT 1"
            );
            let response = self
                .db
                .query(&query)
                .bind(("ctx_node", ctx_node_id.clone()))
                .bind(("task_exec_node", task_exec_node.clone()))
                .bind(("plan_id", plan_id.to_string()))
                .bind(("step_id", step_id.to_string()))
                .await
                .map_err(map_surreal_error)?;
            let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
            if !rows.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // -----------------------------------------------------------------------
    // Planning query helpers
    // -----------------------------------------------------------------------

    async fn query_plan_steps_for_plans(
        &self,
        task_id: &TaskId,
        plan_ids: &[String],
    ) -> Result<HashMap<String, Vec<PlanningPlanStepRecord>>> {
        let mut out: HashMap<String, Vec<PlanningPlanStepRecord>> = HashMap::new();
        if plan_ids.is_empty() {
            return Ok(out);
        }
        let plan_node_ids: Vec<String> = plan_ids
            .iter()
            .map(|plan_id| plan_entity_id_string(task_id, plan_id))
            .collect();
        let query = format!(
            "SELECT props, props.a2a_step_order AS step_order, props.a2a_plan_id AS plan_id \
             FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id IN $plan_node_ids \
                 AND rel_type = '{derived}' \
                 AND from_label = 'PlanStep'\
             ) ORDER BY step_order ASC",
            derived = crate::vocabulary::prov_relations::WAS_DERIVED_FROM,
        );
        let response = self
            .db
            .query(&query)
            .bind(("plan_node_ids", plan_node_ids))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let plan_id = row
                .get("plan_id")
                .and_then(Value::as_str)
                .or_else(|| props.get("a2a_plan_id").and_then(Value::as_str));
            let Some(plan_id) = plan_id else {
                continue;
            };
            let step_id = props.get("a2a_step_id").and_then(Value::as_str);
            let description = props
                .get("prov_label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let step_order = props
                .get("a2a_step_order")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let depends_on_raw = props
                .get("a2a_depends_on")
                .and_then(Value::as_str)
                .map(String::from);
            let step_status = props
                .get("a2a_status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let Some(step_id) = step_id else {
                continue;
            };
            out.entry(plan_id.to_string())
                .or_default()
                .push(PlanningPlanStepRecord {
                    step_id: step_id.to_string(),
                    description: description.to_string(),
                    order: step_order.max(0) as u32,
                    depends_on: decode_depends_on(depends_on_raw),
                    status: step_status.to_string(),
                });
        }
        Ok(out)
    }

    #[allow(dead_code)]
    async fn query_plan_steps(
        &self,
        task_id: &TaskId,
        plan_id: &str,
    ) -> Result<Vec<PlanningPlanStepRecord>> {
        Ok(self
            .query_plan_steps_for_plans(task_id, &[plan_id.to_string()])
            .await?
            .remove(plan_id)
            .unwrap_or_default())
    }
}

// The historical `task_agent_id_cache_tests` module was removed
// alongside the process-local cache: `get_task_agent_id` now performs
// a single edge hop via `WAS_LAST_EXECUTED_BY`, so cache hit-rate is
// no longer part of the public contract. Head-pointer cardinality is
// covered by `head_pointer_cardinality_test` under `tests/`.
