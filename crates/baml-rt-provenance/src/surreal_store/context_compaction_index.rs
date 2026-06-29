// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Write-maintained compaction head per context (optionally task-scoped).

use baml_rt_core::ids::{ActivityAnchorId, ContextId, TaskId};
use serde_json::Value;

use super::SurrealProvenanceStore;
use crate::{
    context_compaction::types::{ContextCompactionHead, ContextCompactionTrigger},
    error::Result,
    events::{ProvEvent, ProvEventData},
    surreal_store::helpers::{check_and_take_zero, map_surreal_error},
    surreal_tables::TBL_CONTEXT_COMPACTION_INDEX,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ContextCompactionIndexRow {
    pub context_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_entity_id: Option<String>,
    pub activity_anchor: String,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub summary_text: String,
    pub trigger: String,
    pub event_order: u64,
}

impl SurrealProvenanceStore {
    pub(crate) async fn upsert_context_compaction_index(&self, event: &ProvEvent) -> Result<()> {
        let ProvEventData::ContextCompactionRecorded {
            task_id,
            covered_event_order_start,
            covered_event_order_end,
            summary_text,
            trigger,
            ..
        } = event.data()
        else {
            return Ok(());
        };
        let Some(context_id) = event.context_id_opt() else {
            return Ok(());
        };
        let row = ContextCompactionIndexRow {
            context_id: context_id.as_str().to_string(),
            task_entity_id: task_id.as_ref().map(|t| format!("task:{}", t.as_str())),
            activity_anchor: event.id().as_str().to_string(),
            covered_event_order_start: *covered_event_order_start,
            covered_event_order_end: *covered_event_order_end,
            summary_text: summary_text.clone(),
            trigger: trigger.as_wire_str().to_string(),
            event_order: event.timestamp_ms(),
        };
        let sql = format!(
            "UPSERT {TBL_CONTEXT_COMPACTION_INDEX} SET \
               context_id = $context_id, \
               task_entity_id = $task_entity_id, \
               activity_anchor = $activity_anchor, \
               covered_event_order_start = $covered_event_order_start, \
               covered_event_order_end = $covered_event_order_end, \
               summary_text = $summary_text, \
               trigger = $trigger, \
               event_order = $event_order \
             WHERE context_id = $context_id \
               AND task_entity_id {} $task_entity_id",
            if row.task_entity_id.is_some() {
                "="
            } else {
                "IS"
            }
        );
        self.db()
            .query(&sql)
            .bind(("context_id", row.context_id.clone()))
            .bind(("task_entity_id", row.task_entity_id.clone()))
            .bind(("activity_anchor", row.activity_anchor.clone()))
            .bind(("covered_event_order_start", row.covered_event_order_start))
            .bind(("covered_event_order_end", row.covered_event_order_end))
            .bind(("summary_text", row.summary_text.clone()))
            .bind(("trigger", row.trigger.clone()))
            .bind(("event_order", row.event_order))
            .await
            .map_err(map_surreal_error)?
            .check()
            .map_err(map_surreal_error)?;
        Ok(())
    }

    pub async fn latest_compaction_head(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
    ) -> Result<Option<ContextCompactionHead>> {
        let ctx = context_id.as_str();
        let task_entity = task_id.map(|t| format!("task:{}", t.as_str()));
        let query = if task_entity.is_some() {
            format!(
                "SELECT * FROM {TBL_CONTEXT_COMPACTION_INDEX} \
                 WHERE context_id = $ctx AND task_entity_id = $task \
                 ORDER BY event_order DESC LIMIT 1"
            )
        } else {
            format!(
                "SELECT * FROM {TBL_CONTEXT_COMPACTION_INDEX} \
                 WHERE context_id = $ctx AND task_entity_id IS NONE \
                 ORDER BY event_order DESC LIMIT 1"
            )
        };
        let mut q = self.db().query(&query).bind(("ctx", ctx.to_string()));
        if let Some(task) = task_entity {
            q = q.bind(("task", task));
        }
        let response = q.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.into_iter().next().and_then(|row| {
            Some(ContextCompactionHead {
                activity_anchor: ActivityAnchorId::from(row.get("activity_anchor")?.as_str()?),
                covered_event_order_start: row.get("covered_event_order_start")?.as_u64()?,
                covered_event_order_end: row.get("covered_event_order_end")?.as_u64()?,
                summary_text: row.get("summary_text")?.as_str()?.to_string(),
                trigger: parse_trigger(row.get("trigger")?.as_str()?),
                event_order: row.get("event_order")?.as_u64()?,
                task_entity_id: row
                    .get("task_entity_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        }))
    }
}

fn parse_trigger(raw: &str) -> ContextCompactionTrigger {
    match raw {
        "pre_model_emergency" => ContextCompactionTrigger::PreModelEmergency,
        "manual_operator" => ContextCompactionTrigger::ManualOperator,
        _ => ContextCompactionTrigger::PostTurnThreshold,
    }
}
