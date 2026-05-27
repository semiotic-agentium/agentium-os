//! Transcript projection: operational failures, planning lifecycle, merged into operator timeline.

use std::collections::HashSet;

use baml_rt_conversation::{
    operational::{OperationalEventContent, OperationalEventKind, OperationalEventSeverity},
    planning::{PlanningEventContent, PlanningEventKind},
    view::{ConversationItemContent, ProvenanceConversationContextItem},
};
use baml_rt_core::ids::{ActivityAnchorId, ContextId, TaskId};
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::Result,
    id_semantics::{
        context_entity_id_string, task_entity_id_string_raw, task_execution_activity_id_string,
    },
    observation::EventOrder,
    store::ProvenancePlanningQuery,
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{a2a_relations, context_scope, semantic_labels},
};

fn prop_str(props: &Value, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn is_terminal_task_status(raw: &str) -> bool {
    let n = raw.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "task_state_completed"
            | "completed"
            | "task_state_failed"
            | "failed"
            | "task_state_canceled"
            | "canceled"
            | "task_state_cancelled"
            | "cancelled"
            | "task_state_rejected"
            | "rejected"
    )
}

impl SurrealProvenanceStore {
    /// Operational + planning rows for the operator transcript (replaces operational supplement).
    pub(super) async fn load_transcript_extension_items(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
        after_event_order: Option<EventOrder>,
        existing_anchors: &HashSet<String>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let mut items = self
            .load_operational_transcript_items(
                context_id,
                task_id,
                after_event_order,
                existing_anchors,
            )
            .await?;
        if let Some(tid) = task_id {
            let mut planning = self
                .load_planning_transcript_items(context_id, tid, existing_anchors)
                .await?;
            items.append(&mut planning);
        }
        items.sort_by(crate::observation::cmp_transcript_items);
        Ok(items)
    }

    async fn load_operational_transcript_items(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
        after_event_order: Option<EventOrder>,
        existing_anchors: &HashSet<String>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let ctx_node_id = context_entity_id_string(context_id.as_str());
        let scoped_to = context_scope::SCOPED_TO;

        let after_filter_sql = match after_event_order {
            Some(_) => crate::observation::after_event_order_filter_sql(),
            None => "",
        };

        let task_call_filter = match task_id {
            None => String::new(),
            Some(_) => {
                let tc = a2a_relations::TASK_CALL;
                format!(
                    "AND label IN ['LlmCall', 'PromptRejected'] AND node_id IN (\
                       SELECT VALUE to_id FROM {TBL_EDGE} \
                       WHERE from_id = $task_exec_id AND rel_type = '{tc}'\
                     )"
                )
            }
        };
        let task_state_filter = match task_id {
            None => String::new(),
            Some(_) => {
                let wlt = semantic_labels::WAS_LAST_TRANSITIONED_TO;
                format!(
                    "AND label = 'TaskState' AND node_id IN (\
                       SELECT VALUE to_id FROM {TBL_EDGE} \
                       WHERE from_id = $task_entity_id AND rel_type = '{wlt}'\
                     )"
                )
            }
        };

        let base_sql = format!(
            "SELECT node_id, label, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $ctx_node_id AND rel_type = '{scoped_to}' \
                 AND from_label IN ['LlmCall', 'PromptRejected', 'TaskState']\
             ) \
             {after_filter_sql} \
             {{task_filter}} \
             ORDER BY event_order ASC, node_id ASC"
        );

        let mut rows: Vec<Value> = Vec::new();
        if task_id.is_some() {
            for task_filter_sql in [task_call_filter.as_str(), task_state_filter.as_str()] {
                let sql = base_sql.replace("{task_filter}", task_filter_sql);
                let mut q = self.db.query(&sql);
                q = q.bind(("ctx_node_id", ctx_node_id.clone()));
                if let Some(tid) = task_id {
                    q = q.bind(("task_entity_id", task_entity_id_string_raw(tid.as_str())));
                    q = q.bind((
                        "task_exec_id",
                        task_execution_activity_id_string(tid.as_str()),
                    ));
                }
                if let Some(after) = after_event_order {
                    q = q.bind(("after_event_order", after.as_u64()));
                }
                let response = q.await.map_err(map_surreal_error)?;
                rows.extend(check_and_take_zero(response, map_surreal_error)?);
            }
            rows.sort_by(|a, b| {
                let key = |row: &Value| {
                    (
                        row.get("event_order").and_then(Value::as_u64).unwrap_or(0),
                        row.get("node_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                };
                key(a).cmp(&key(b))
            });
        } else {
            let sql = base_sql.replace("{task_filter}", "");
            let mut q = self.db.query(&sql);
            q = q.bind(("ctx_node_id", ctx_node_id));
            if let Some(after) = after_event_order {
                q = q.bind(("after_event_order", after.as_u64()));
            }
            let response = q.await.map_err(map_surreal_error)?;
            rows = check_and_take_zero(response, map_surreal_error)?;
        }

        let mut failed_llm_anchors: Vec<String> = Vec::new();
        for row in &rows {
            let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
            if label != "LlmCall" {
                continue;
            }
            let props = row.get("props").cloned().unwrap_or(Value::Null);
            if prop_str(&props, "a2a_activity_outcome") != Some("Failed".to_string()) {
                continue;
            }
            if let Some(anchor) = prop_str(&props, "a2a_activity_anchor") {
                failed_llm_anchors.push(anchor);
            }
        }

        let failure_map: std::collections::HashMap<String, (String, String)> = self
            .load_failure_classification_for_activity_ids(&failed_llm_anchors)
            .await?;

        let mut items = Vec::new();
        for row in rows {
            let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
            let props = row.get("props").cloned().unwrap_or(Value::Null);
            let event_id = prop_str(&props, "a2a_activity_anchor").unwrap_or_default();
            if event_id.is_empty() || existing_anchors.contains(&event_id) {
                continue;
            }
            let timestamp_ms = row.get("event_order").and_then(Value::as_u64).unwrap_or(0);

            let content = match label {
                "LlmCall" => {
                    if prop_str(&props, "a2a_activity_outcome") != Some("Failed".to_string()) {
                        continue;
                    }
                    let function_name =
                        prop_str(&props, "a2a_function_name").unwrap_or_else(|| "LLM call".into());
                    let (failure_class, failure_evidence) =
                        failure_map.get(&event_id).cloned().unwrap_or_else(|| {
                            (
                                "failed_graph_incomplete".to_string(),
                                "llm_call_failed".to_string(),
                            )
                        });
                    let summary = format!("LLM {function_name} failed");
                    OperationalEventContent {
                        kind: OperationalEventKind::LlmCallFailed,
                        severity: OperationalEventSeverity::Error,
                        summary,
                        detail: Some(failure_evidence.clone()),
                        agent_package: None,
                        agent_instance_id: None,
                        failure_class: Some(failure_class),
                        failure_evidence: Some(failure_evidence),
                        old_status: None,
                        new_status: None,
                    }
                }
                "PromptRejected" => {
                    let reason =
                        prop_str(&props, "a2a_reason").unwrap_or_else(|| "rejected".into());
                    OperationalEventContent {
                        kind: OperationalEventKind::PromptRejected,
                        severity: OperationalEventSeverity::Error,
                        summary: format!("Prompt rejected: {reason}"),
                        detail: Some(reason.clone()),
                        agent_package: None,
                        agent_instance_id: None,
                        failure_class: Some("prompt_rejected".to_string()),
                        failure_evidence: Some(reason),
                        old_status: None,
                        new_status: None,
                    }
                }
                "TaskState" => {
                    let new_status =
                        prop_str(&props, "a2a_task_state").unwrap_or_else(|| "unknown".into());
                    if !is_terminal_task_status(&new_status) {
                        continue;
                    }
                    let old_status = prop_str(&props, "a2a_old_status");
                    let summary = match old_status.as_ref() {
                        Some(old) => format!("Task status: {old} → {new_status}"),
                        None => format!("Task status: {new_status}"),
                    };
                    OperationalEventContent {
                        kind: OperationalEventKind::TaskStatusChanged,
                        severity: OperationalEventSeverity::Warning,
                        summary,
                        detail: prop_str(&props, "a2a_reason"),
                        agent_package: None,
                        agent_instance_id: None,
                        failure_class: None,
                        failure_evidence: None,
                        old_status,
                        new_status: Some(new_status),
                    }
                }
                _ => continue,
            };

            items.push(ProvenanceConversationContextItem {
                timestamp_ms,
                activity_anchor: ActivityAnchorId::from(event_id),
                role: "system".to_string(),
                content: ConversationItemContent::Operational(content),
                user_speaker_kind: None,
            });
        }
        Ok(items)
    }

    async fn load_planning_transcript_items(
        &self,
        _context_id: &ContextId,
        task_id: &TaskId,
        existing_anchors: &HashSet<String>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let mut items = Vec::new();

        let intent = self.query_current_intent(task_id).await?;

        if let Some(intent) = intent {
            let anchor = intent.activity_anchor_id.as_str().to_string();
            if !existing_anchors.contains(&anchor) {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: intent.event_order,
                    activity_anchor: intent.activity_anchor_id.clone(),
                    role: "system".to_string(),
                    content: ConversationItemContent::Planning(PlanningEventContent {
                        kind: PlanningEventKind::IntentResolved,
                        summary: format!("Intent: {}", intent.description),
                        detail: None,
                        intent_id: Some(intent.intent_id.clone()),
                        plan_id: None,
                        step_id: None,
                        old_status: None,
                        new_status: Some("resolved".to_string()),
                    }),
                    user_speaker_kind: None,
                });
            }
        }

        let plan = self.query_current_plan(task_id).await?;

        if let Some(plan) = plan {
            let plan_anchor = plan.activity_anchor_id.as_str().to_string();
            if !existing_anchors.contains(&plan_anchor) {
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: plan.event_order,
                    activity_anchor: plan.activity_anchor_id.clone(),
                    role: "system".to_string(),
                    content: ConversationItemContent::Planning(PlanningEventContent {
                        kind: PlanningEventKind::PlanCommitted,
                        summary: format!("Plan committed ({})", plan.plan_id),
                        detail: None,
                        intent_id: Some(plan.intent_id.clone()),
                        plan_id: Some(plan.plan_id.clone()),
                        step_id: None,
                        old_status: None,
                        new_status: None,
                    }),
                    user_speaker_kind: None,
                });
            }

            for step in &plan.steps {
                let step_anchor = format!(
                    "plan-step:{}:{}:{}",
                    task_id.as_str(),
                    plan.plan_id,
                    step.step_id
                );
                if existing_anchors.contains(&step_anchor) {
                    continue;
                }
                items.push(ProvenanceConversationContextItem {
                    timestamp_ms: plan.event_order.saturating_add(step.order as u64),
                    activity_anchor: ActivityAnchorId::from(step_anchor),
                    role: "system".to_string(),
                    content: ConversationItemContent::Planning(PlanningEventContent {
                        kind: PlanningEventKind::PlanStepStatusChanged,
                        summary: format!("Step {}: {}", step.step_id, step.status),
                        detail: Some(step.description.clone()),
                        intent_id: Some(plan.intent_id.clone()),
                        plan_id: Some(plan.plan_id.clone()),
                        step_id: Some(step.step_id.clone()),
                        old_status: None,
                        new_status: Some(step.status.clone()),
                    }),
                    user_speaker_kind: None,
                });
            }
        }

        Ok(items)
    }
}
