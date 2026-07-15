// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared predicates for provenance ops row maps (tool/LLM query responses).

use baml_rt_conversation::view::ToolSessionPhase;
use serde_json::{Map, Value};

pub(crate) fn ops_row_has_recorded_gate(row: &Map<String, Value>) -> bool {
    row.get("gate").is_some() || row.get("a2a_gate").is_some()
}

pub(crate) fn ops_row_is_terminal_outcome(row: &Map<String, Value>) -> bool {
    matches!(
        row.get("activity_outcome").and_then(Value::as_str),
        Some("Failed") | Some("Success")
    )
}

/// Session `open` markers are bookkeeping until send/read completes; hide them unless terminal.
pub(crate) fn tool_call_ops_row_visible(row: &Map<String, Value>) -> bool {
    let Some(phase_str) = row
        .get("tool_call")
        .and_then(|v| v.get("phase"))
        .and_then(Value::as_str)
    else {
        return true;
    };
    let phase = ToolSessionPhase::from_metadata(&serde_json::json!({ "phase": phase_str }));
    if !matches!(phase, ToolSessionPhase::Open) {
        return true;
    }
    ops_row_is_terminal_outcome(row) || ops_row_has_recorded_gate(row)
}
