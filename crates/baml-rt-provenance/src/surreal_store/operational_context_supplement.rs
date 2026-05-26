//! Supplemental operational transcript rows (LLM failures, prompt rejected, task status).

use std::collections::HashSet;

use baml_rt_conversation::{
    operational::{OperationalEventContent, OperationalEventKind, OperationalEventSeverity},
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
    id_semantics::context_entity_id_string,
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::context_scope,
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
    pub(super) async fn load_operational_supplement_items(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
        after_event_order: Option<u64>,
        existing_anchors: &HashSet<String>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        if task_id.is_some() {
            return Ok(Vec::new());
        }
        let ctx_node_id = context_entity_id_string(context_id.as_str());
        let scoped_to = context_scope::SCOPED_TO;

        let after_filter_sql = match after_event_order {
            Some(_) => "AND props.a2a_event_order > $after_event_order",
            None => "",
        };

        let sql = format!(
            "SELECT node_id, label, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $ctx_node_id AND rel_type = '{scoped_to}' \
                 AND from_label IN ['LlmCall', 'PromptRejected', 'TaskState']\
             ) \
             {after_filter_sql} \
             ORDER BY event_order ASC, node_id ASC"
        );

        let mut q = self.db.query(&sql);
        q = q.bind(("ctx_node_id", ctx_node_id));
        if let Some(after) = after_event_order {
            q = q.bind(("after_event_order", after));
        }
        let response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

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

        let failure_map = self
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
}
