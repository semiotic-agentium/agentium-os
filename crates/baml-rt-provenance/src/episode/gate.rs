// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Gate decision aggregation for planning / episode surfaces.

use baml_rt_core::ids::{ContextId, TaskId};

use super::gate_row::{
    DeniedSnapshot, parse_gate_tool_row, resolve_telemetry_verdict, sort_gate_rows_chronologically,
};
use crate::{
    error::Result,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    },
};

#[derive(Debug, Clone)]
pub struct TaskGateAggregate {
    pub deny_count: u32,
    pub ask_count: u32,
    pub pass_gated_count: u32,
    pub pass_count: u32,
    pub prevented_error_count: u32,
    pub friction_denial_count: u32,
    pub gate_events: Vec<GateEventRow>,
}

#[derive(Debug, Clone)]
pub struct GateEventRow {
    pub tool_name: String,
    pub tier: u8,
    pub decision: String,
    pub reason_code: String,
    pub deficient_nodes: Vec<String>,
    pub tool_call_anchor: String,
    pub telemetry_verdict: Option<String>,
}

pub async fn aggregate_task_gate(
    store: &dyn ProvenanceOpsQuery,
    context_id: &ContextId,
    task_id: &TaskId,
) -> Result<Option<TaskGateAggregate>> {
    let report = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
                task_id: Some(task_id.clone()),
                ..Default::default()
            },
            response_profile: Some(crate::store::ProvenanceResponseProfile::ToolCompact),
            page_size: Some(500),
            sort_by: Some("timestamp_ms".to_string()),
            sort_dir: Some("asc".to_string()),
            ..Default::default()
        })
        .await?;

    let mut rows = report.rows;
    sort_gate_rows_chronologically(&mut rows);

    let mut deny_count = 0u32;
    let mut ask_count = 0u32;
    let mut pass_gated_count = 0u32;
    let mut pass_count = 0u32;
    let mut prevented_error_count = 0u32;
    let mut friction_denial_count = 0u32;
    let mut gate_events = Vec::new();
    let mut recent_denies: Vec<DeniedSnapshot> = Vec::new();

    for row in &rows {
        let Some(parsed) = parse_gate_tool_row(row) else {
            continue;
        };
        let (telemetry_verdict, friction, prevented) =
            resolve_telemetry_verdict(&parsed, &mut recent_denies);
        if friction {
            friction_denial_count += 1;
        }
        if prevented {
            prevented_error_count += 1;
        }

        match parsed.decision.as_str() {
            "deny" => {
                deny_count += 1;
                recent_denies.push(DeniedSnapshot {
                    tool_name: parsed.tool_name.clone(),
                    args: parsed.args.clone(),
                });
            }
            "ask" => ask_count += 1,
            "pass_gated" => pass_gated_count += 1,
            "pass" => pass_count += 1,
            _ => {}
        }

        gate_events.push(GateEventRow {
            tool_name: parsed.tool_name,
            tier: parsed.tier,
            decision: parsed.decision,
            reason_code: parsed.reason_code,
            deficient_nodes: parsed.deficient_nodes,
            tool_call_anchor: parsed.tool_call_anchor,
            telemetry_verdict,
        });
    }

    if gate_events.is_empty() {
        return Ok(None);
    }

    Ok(Some(TaskGateAggregate {
        deny_count,
        ask_count,
        pass_gated_count,
        pass_count,
        prevented_error_count,
        friction_denial_count,
        gate_events,
    }))
}
