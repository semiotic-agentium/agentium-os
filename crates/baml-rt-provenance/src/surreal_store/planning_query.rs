//! [`ProvenancePlanningQuery`] and planning graph helpers (intents, plans, step gate).

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use baml_rt_core::{
    bus::PlanningSupersessionKind,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{decode_depends_on, is_step_completed_status, map_surreal_error, query_take_zero},
};
use crate::{
    error::{ProvenanceError, Result},
    events::ProvEventData,
    id_semantics::context_entity_id_string,
    normalizer::{plan_entity_id_string, task_entity_id_string},
    store::{
        PlanningIntentRecord, PlanningPlanRecord, PlanningPlanStepRecord, ProvenancePlanningQuery,
    },
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{context_scope, semantic_labels},
};

#[async_trait]
impl ProvenancePlanningQuery for SurrealProvenanceStore {
    async fn query_current_intent(&self, task_id: &TaskId) -> Result<Option<PlanningIntentRecord>> {
        let intents = self.query_intent_history(task_id, Some(500)).await?;
        if intents.is_empty() {
            return Ok(None);
        }
        // Find intents that are superseded (have outgoing WAS_REPLACED_BY or WAS_REFINED_BY)
        let replaced_sources = self
            .collect_superseded_activity_anchors(task_id, "Intent")
            .await?;
        Ok(intents
            .into_iter()
            .find(|intent| !replaced_sources.contains(intent.activity_anchor_id.as_str())))
    }

    async fn query_current_plan(&self, task_id: &TaskId) -> Result<Option<PlanningPlanRecord>> {
        let plans = self.query_plan_history(task_id, Some(500)).await?;
        if plans.is_empty() {
            return Ok(None);
        }
        let replaced_sources = self
            .collect_superseded_activity_anchors(task_id, "Plan")
            .await?;
        Ok(plans
            .into_iter()
            .find(|plan| !replaced_sources.contains(plan.activity_anchor_id.as_str())))
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
             ) ORDER BY event_order DESC",
            has_intent = semantic_labels::HAS_INTENT,
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_node_id", task_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let (intent_incoming, intent_outgoing) =
            self.query_supersession_maps("Intent", task_id).await?;

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
                supersession_from_previous: intent_incoming.get(event_id).copied(),
                superseded_by_next: intent_outgoing.get(event_id).copied(),
            });
        }
        intents.sort_by_key(|r| std::cmp::Reverse(r.event_order));
        if intents.len() > limit_val {
            intents.truncate(limit_val);
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
             ) ORDER BY event_order DESC",
            has_plan = semantic_labels::HAS_PLAN,
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_node_id", task_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let (plan_incoming, plan_outgoing) = self.query_supersession_maps("Plan", task_id).await?;

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
            let steps = self.query_plan_steps(task_id, plan_id).await?;
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
                supersession_from_previous: plan_incoming.get(event_id).copied(),
                superseded_by_next: plan_outgoing.get(event_id).copied(),
            });
        }
        plans.sort_by_key(|r| std::cmp::Reverse(r.event_order));
        if plans.len() > limit_val {
            plans.truncate(limit_val);
        }
        Ok(plans)
    }
}

impl SurrealProvenanceStore {
    // -----------------------------------------------------------------------
    // Graph traversal helpers
    // -----------------------------------------------------------------------

    pub async fn get_task_agent_id(&self, task_id: &TaskId) -> Result<Option<AgentId>> {
        let task_entity_id = task_entity_id_string(task_id);
        // Two-hop traversal: Task -[WAS_CREATED_BY]-> TaskExecution -[WAS_EXECUTED_BY]-> AgentInstance
        // then read props.a2a_agent_id from the agent instance node, all in one query.
        let query = format!(
            "SELECT node_id, props.a2a_agent_id AS agent_id OMIT id FROM {TBL_NODE} \
             WHERE node_id = (\
               SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id = (\
                 SELECT VALUE to_id FROM {TBL_EDGE} \
                 WHERE from_id = $task_node AND rel_type = $rel_created LIMIT 1\
               )[0] \
               AND rel_type = $rel_executed LIMIT 1\
             )[0] \
             LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_node", task_entity_id))
            .bind(("rel_created", semantic_labels::WAS_CREATED_BY))
            .bind(("rel_executed", semantic_labels::WAS_EXECUTED_BY))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let agent_id_str = row.get("agent_id").and_then(Value::as_str);
        let Some(agent_id_str) = agent_id_str else {
            return Ok(None);
        };
        if agent_id_str.trim().is_empty() {
            return Ok(None);
        }
        UuidId::parse_str(agent_id_str)
            .map(AgentId::from_uuid)
            .map(Some)
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
        let mut response = self
            .db
            .query(&query)
            .bind(("step_node_id", step_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let mut response = self
            .db
            .query(&query)
            .bind(("step_node_id", step_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
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
        let query = format!(
            "SELECT node_id FROM {TBL_NODE} \
             WHERE (label = 'LlmCall' OR label = 'ToolCall') \
               AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
                 WHERE to_id = $ctx_node AND rel_type = '{scoped}') \
               AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
                 WHERE from_id = $task_exec_node AND rel_type = '{task_call}') \
               AND props.a2a_plan_id = $plan_id \
               AND props.a2a_step_id = $step_id \
               AND props.a2a_activity_outcome = 'Success' \
             LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("ctx_node", ctx_node_id))
            .bind(("task_exec_node", task_exec_node))
            .bind(("plan_id", plan_id.to_string()))
            .bind(("step_id", step_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;
        Ok(!rows.is_empty())
    }

    // -----------------------------------------------------------------------
    // Planning query helpers
    // -----------------------------------------------------------------------

    async fn query_plan_steps(
        &self,
        task_id: &TaskId,
        plan_id: &str,
    ) -> Result<Vec<PlanningPlanStepRecord>> {
        let plan_node_id = plan_entity_id_string(task_id, plan_id);
        let query = format!(
            "SELECT props, props.a2a_step_order AS step_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $plan_node_id \
                 AND rel_type = '{derived}' \
                 AND from_label = 'PlanStep'\
             ) ORDER BY step_order ASC",
            derived = crate::vocabulary::prov_relations::WAS_DERIVED_FROM,
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("plan_node_id", plan_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        let mut steps = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
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
            steps.push(PlanningPlanStepRecord {
                step_id: step_id.to_string(),
                description: description.to_string(),
                order: step_order.max(0) as u32,
                depends_on: decode_depends_on(depends_on_raw),
                status: step_status.to_string(),
            });
        }
        Ok(steps)
    }

    async fn query_supersession_maps(
        &self,
        node_label: &str,
        task_id: &TaskId,
    ) -> Result<(
        HashMap<String, PlanningSupersessionKind>,
        HashMap<String, PlanningSupersessionKind>,
    )> {
        // Query WAS_REPLACED_BY and WAS_REFINED_BY edges concurrently — independent queries.
        let (replaced_edges, refined_edges) = tokio::try_join!(
            self.query_supersession_edges(node_label, task_id, semantic_labels::WAS_REPLACED_BY),
            self.query_supersession_edges(node_label, task_id, semantic_labels::WAS_REFINED_BY),
        )?;

        let mut incoming: HashMap<String, PlanningSupersessionKind> = HashMap::new();
        let mut outgoing: HashMap<String, PlanningSupersessionKind> = HashMap::new();

        for (source_anchor, target_anchor) in &replaced_edges {
            incoming
                .entry(target_anchor.clone())
                .or_insert(PlanningSupersessionKind::ReplacedBy);
            outgoing
                .entry(source_anchor.clone())
                .or_insert(PlanningSupersessionKind::ReplacedBy);
        }
        for (source_anchor, target_anchor) in &refined_edges {
            incoming
                .entry(target_anchor.clone())
                .or_insert(PlanningSupersessionKind::RefinedBy);
            outgoing
                .entry(source_anchor.clone())
                .or_insert(PlanningSupersessionKind::RefinedBy);
        }

        Ok((incoming, outgoing))
    }

    async fn query_supersession_edges(
        &self,
        node_label: &str,
        task_id: &TaskId,
        rel_type: &str,
    ) -> Result<Vec<(String, String)>> {
        let ownership_edge = match node_label {
            "Intent" => semantic_labels::HAS_INTENT,
            "Plan" => semantic_labels::HAS_PLAN,
            _ => return Ok(vec![]),
        };
        let task_node_id = task_entity_id_string(task_id);
        let query = format!(
            "SELECT from_id, to_id FROM {TBL_EDGE} \
             WHERE rel_type = $rel_type AND from_label = $label AND to_label = $label \
               AND from_id IN (SELECT VALUE to_id FROM {TBL_EDGE} WHERE from_id = $task_node_id AND rel_type = $ownership_edge) \
               AND to_id IN (SELECT VALUE to_id FROM {TBL_EDGE} WHERE from_id = $task_node_id AND rel_type = $ownership_edge)"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("rel_type", rel_type.to_string()))
            .bind(("label", node_label.to_string()))
            .bind(("task_node_id", task_node_id))
            .bind(("ownership_edge", ownership_edge.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = query_take_zero(&mut response, map_surreal_error)?;

        // Collect all referenced node IDs, then batch-fetch activity anchors in one query.
        let mut node_ids: Vec<String> = Vec::new();
        let mut edge_pairs: Vec<(String, String)> = Vec::new();
        for row in &rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or_default();
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            node_ids.push(from_id.to_string());
            node_ids.push(to_id.to_string());
            edge_pairs.push((from_id.to_string(), to_id.to_string()));
        }

        if edge_pairs.is_empty() {
            return Ok(Vec::new());
        }

        node_ids.sort_unstable();
        node_ids.dedup();

        let anchor_query = format!(
            "SELECT node_id, props.a2a_activity_anchor AS anchor FROM {TBL_NODE} WHERE node_id IN $ids"
        );
        let mut anchor_response = self
            .db
            .query(&anchor_query)
            .bind(("ids", node_ids))
            .await
            .map_err(map_surreal_error)?;
        let anchor_rows: Vec<Value> = query_take_zero(&mut anchor_response, map_surreal_error)?;

        let anchor_map: HashMap<String, String> = anchor_rows
            .iter()
            .filter_map(|r| {
                let nid = r.get("node_id").and_then(Value::as_str)?;
                let anchor = r.get("anchor").and_then(Value::as_str)?;
                if anchor.is_empty() {
                    return None;
                }
                Some((nid.to_string(), anchor.to_string()))
            })
            .collect();

        let mut results = Vec::new();
        for (from_id, to_id) in edge_pairs {
            if let (Some(from_event), Some(to_event)) =
                (anchor_map.get(&from_id), anchor_map.get(&to_id))
            {
                results.push((from_event.clone(), to_event.clone()));
            }
        }
        Ok(results)
    }

    async fn collect_superseded_activity_anchors(
        &self,
        task_id: &TaskId,
        node_label: &str,
    ) -> Result<HashSet<String>> {
        let (replaced_edges, refined_edges) = tokio::try_join!(
            self.query_supersession_edges(node_label, task_id, semantic_labels::WAS_REPLACED_BY),
            self.query_supersession_edges(node_label, task_id, semantic_labels::WAS_REFINED_BY),
        )?;
        let mut superseded = HashSet::new();
        for (source_anchor, _) in replaced_edges {
            superseded.insert(source_anchor);
        }
        for (source_anchor, _) in refined_edges {
            superseded.insert(source_anchor);
        }
        Ok(superseded)
    }
}
