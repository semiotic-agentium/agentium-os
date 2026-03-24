//! Defaults and JSON-schema checks for tool `open_input` (session Open / auto-open policy).

use serde_json::Value;

/// Default `open_input` when none is provided.
pub(crate) fn empty_open_input() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Whether the tool's `open_input` JSON Schema allows an empty object open (for strict auto-open).
pub(crate) fn schema_allows_empty_open_input(schema: &Value) -> bool {
    match schema {
        Value::Null => true,
        Value::Object(map) => {
            if let Some(any_of) = map.get("anyOf").and_then(Value::as_array)
                && any_of.iter().any(schema_allows_empty_open_input)
            {
                return true;
            }
            if let Some(one_of) = map.get("oneOf").and_then(Value::as_array)
                && one_of.iter().any(schema_allows_empty_open_input)
            {
                return true;
            }
            if let Some(all_of) = map.get("allOf").and_then(Value::as_array)
                && !all_of.is_empty()
                && all_of.iter().all(schema_allows_empty_open_input)
            {
                return true;
            }

            let type_allows = match map.get("type") {
                Some(Value::String(t)) if t == "null" => return true,
                Some(Value::String(t)) if t == "object" => true,
                Some(Value::Array(types)) => types
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|t| t == "object" || t == "null"),
                Some(_) => false,
                None => map.contains_key("properties") || map.contains_key("required"),
            };
            if !type_allows {
                return false;
            }

            let has_required = map
                .get("required")
                .and_then(Value::as_array)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if has_required {
                return false;
            }
            let min_properties = map
                .get("minProperties")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            min_properties == 0
        }
        _ => false,
    }
}

/// Provenance / planning: step statuses treated as terminal “completed”.
pub(crate) fn is_planning_step_terminal_completed_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "done" | "step_completed" | "finished"
    )
}
