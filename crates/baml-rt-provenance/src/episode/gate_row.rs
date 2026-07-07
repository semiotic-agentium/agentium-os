// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared parsing and deny-stack telemetry matching for gate tool_call ops rows.

use baml_rt_semiotic::denied_recent::{self, TelemetryVerdict};
use serde_json::Value;

use crate::ops_types::ProvenanceOpsRow;

#[derive(Debug, Clone)]
pub struct ParsedGateToolCall {
    pub agent_package: String,
    pub context_id: String,
    pub task_id: String,
    pub occurred_at_ms: u64,
    pub decision: String,
    pub tool_name: String,
    pub tier: u8,
    pub args: Value,
    pub reason_code: String,
    pub deficient_nodes: Vec<String>,
    pub tool_call_anchor: String,
    pub stored_telemetry_verdict: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DeniedSnapshot {
    pub(crate) tool_name: String,
    pub(crate) args: Value,
}

/// Parse a provenance ops row into gate fields when a gate decision is present.
pub fn parse_gate_tool_row(row: &ProvenanceOpsRow) -> Option<ParsedGateToolCall> {
    let row_obj = row.as_map();
    let gate = row_obj.get("gate")?.as_object()?;
    let decision = gate
        .get("decision")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tool_name = gate
        .get("toolName")
        .or_else(|| gate.get("tool_name"))
        .and_then(|v| v.as_str())
        .unwrap_or(
            row_obj
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
        )
        .to_string();
    let tier = gate.get("tier").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
    let args = row_obj.get("args").cloned().unwrap_or(Value::Null);
    let reason_code = gate
        .get("reasonCode")
        .or_else(|| gate.get("reason_code"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let deficient_nodes: Vec<String> = gate
        .get("deficientNodes")
        .or_else(|| gate.get("deficient_nodes"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let stored_telemetry_verdict = gate
        .get("telemetryVerdict")
        .or_else(|| gate.get("telemetry_verdict"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Some(ParsedGateToolCall {
        agent_package: row_obj
            .get("agent_package")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        context_id: row.context_id().unwrap_or("").to_string(),
        task_id: row.task_id().unwrap_or("").to_string(),
        occurred_at_ms: row.timestamp_ms().unwrap_or(0),
        decision,
        tool_name,
        tier,
        args,
        reason_code,
        deficient_nodes,
        tool_call_anchor: row_obj
            .get("a2a_activity_anchor")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        stored_telemetry_verdict,
    })
}

pub(crate) fn match_executed_against_denies(
    tool_name: &str,
    args: &Value,
    denies: &[DeniedSnapshot],
) -> Option<(usize, String)> {
    let mut best: Option<(usize, f32, TelemetryVerdict)> = None;
    for (idx, denied) in denies.iter().enumerate() {
        let Some(verdict) = denied_recent::diff_executed_against_denied(
            &denied.tool_name,
            &denied.args,
            tool_name,
            args,
        ) else {
            continue;
        };
        let score = match &verdict {
            TelemetryVerdict::FrictionDenial { .. } => 1.0,
            TelemetryVerdict::PreventedError { .. } => 0.8,
        };
        if best.as_ref().is_none_or(|(_, s, _)| score > *s) {
            best = Some((idx, score, verdict));
        }
    }
    let (idx, _, verdict) = best?;
    let label = match verdict {
        TelemetryVerdict::FrictionDenial { .. } => "friction_denial",
        TelemetryVerdict::PreventedError { .. } => "prevented_error",
    };
    Some((idx, label.to_string()))
}

/// Resolve telemetry verdict via deny-stack when absent on pass/pass_gated rows.
pub fn resolve_telemetry_verdict(
    parsed: &ParsedGateToolCall,
    recent_denies: &mut Vec<DeniedSnapshot>,
) -> (Option<String>, bool, bool) {
    let mut telemetry_verdict = parsed.stored_telemetry_verdict.clone();
    let mut friction = false;
    let mut prevented = false;
    if telemetry_verdict.is_none()
        && matches!(parsed.decision.as_str(), "pass" | "pass_gated")
        && let Some((idx, verdict)) =
            match_executed_against_denies(&parsed.tool_name, &parsed.args, recent_denies)
    {
        telemetry_verdict = Some(verdict.clone());
        if verdict == "friction_denial" {
            friction = true;
        } else if verdict == "prevented_error" {
            prevented = true;
        }
        recent_denies.remove(idx);
    }
    (telemetry_verdict, friction, prevented)
}

/// Sort rows chronologically ascending so deny→pass telemetry pairing is correct.
pub fn sort_gate_rows_chronologically(rows: &mut [ProvenanceOpsRow]) {
    rows.sort_by(|a, b| {
        a.timestamp_ms()
            .unwrap_or(0)
            .cmp(&b.timestamp_ms().unwrap_or(0))
    });
}
