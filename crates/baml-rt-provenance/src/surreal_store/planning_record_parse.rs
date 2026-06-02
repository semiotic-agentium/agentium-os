// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared Surreal node props → planning record parsing (batch + per-task reads).

use std::collections::HashMap;

use baml_rt_core::{
    bus::PlanningSupersessionKind,
    ids::{ActivityAnchorId, ContextId, ExternalId, TaskId},
};
use baml_rt_vocabulary::vocabulary::a2a;
use serde_json::Value;

use crate::store::{PlanningIntentRecord, PlanningPlanRecord, PlanningPlanStepRecord};

pub(super) fn supersession_kind_from_prop(
    props: &Value,
    key: &str,
) -> Option<PlanningSupersessionKind> {
    props
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| match s {
            "replaced_by" => Some(PlanningSupersessionKind::ReplacedBy),
            "refined_by" => Some(PlanningSupersessionKind::RefinedBy),
            _ => None,
        })
}

pub(super) fn intent_record_from_props(props: &Value) -> Option<PlanningIntentRecord> {
    let context_id = props.get("a2a_context_id").and_then(Value::as_str)?;
    let task_id_value = props.get("a2a_task_id").and_then(Value::as_str)?;
    let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str)?;
    let intent_id = props.get("a2a_intent_id").and_then(Value::as_str)?;
    let description = props
        .get("prov_label")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let event_order = props
        .get("a2a_event_order")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(PlanningIntentRecord {
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
    })
}

pub(super) fn plan_record_from_props(
    task_id_value: &str,
    props: &Value,
    steps_by_task_plan: &HashMap<(String, String), Vec<PlanningPlanStepRecord>>,
) -> Option<PlanningPlanRecord> {
    let context_id = props.get("a2a_context_id").and_then(Value::as_str)?;
    let task_id_prop = props.get("a2a_task_id").and_then(Value::as_str)?;
    if task_id_prop != task_id_value {
        return None;
    }
    let event_id = props.get("a2a_activity_anchor").and_then(Value::as_str)?;
    let intent_id = props.get("a2a_intent_id").and_then(Value::as_str)?;
    let plan_id = props.get("a2a_plan_id").and_then(Value::as_str)?;
    let event_order = props
        .get("a2a_event_order")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let steps = steps_by_task_plan
        .get(&(task_id_value.to_string(), plan_id.to_string()))
        .cloned()
        .unwrap_or_default();
    Some(PlanningPlanRecord {
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
    })
}

pub(super) fn group_intent_rows(
    rows: &[Value],
    limit: usize,
) -> HashMap<String, Vec<PlanningIntentRecord>> {
    let mut grouped: HashMap<String, Vec<PlanningIntentRecord>> = HashMap::new();
    for row in rows {
        let Some(props) = row.get("props") else {
            continue;
        };
        let Some(task_id_value) = props.get("a2a_task_id").and_then(Value::as_str) else {
            continue;
        };
        let entry = grouped.entry(task_id_value.to_string()).or_default();
        if entry.len() >= limit {
            continue;
        }
        if let Some(intent) = intent_record_from_props(props) {
            entry.push(intent);
        }
    }
    grouped
}
