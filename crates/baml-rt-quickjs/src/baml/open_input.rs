// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Defaults and JSON-schema checks for tool `open_input` (session Open / auto-open policy).

use serde_json::Value;

/// Default `open_input` when none is provided.
pub(crate) fn empty_open_input() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Whether the tool's `open_input` JSON Schema allows an empty object open (for strict auto-open).
pub(crate) fn schema_allows_empty_open_input(schema: &Value) -> bool {
    baml_rt_tools::schema_allows_empty_or_optional_open_input(schema)
}

/// Provenance / planning: step statuses treated as terminal “completed”.
pub(crate) fn is_planning_step_terminal_completed_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "done" | "step_completed" | "finished"
    )
}
