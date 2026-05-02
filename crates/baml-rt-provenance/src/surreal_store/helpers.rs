use serde_json::Value;

use crate::error::ProvenanceError;

/// Parse `Value::String(s)` as JSON when possible; otherwise clone the value unchanged.
pub(super) fn json_value_from_embedded_string(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        other => other.clone(),
    }
}

/// Row field that may be a JSON string, object, or array; returns `None` for other shapes.
pub(super) fn parse_json_object_field(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => serde_json::from_str(s).ok(),
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        _ => None,
    }
}

pub(crate) fn map_surreal_error(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

/// Validate every statement in a SurrealDB [`surrealdb::IndexedResults`] via
/// [`surrealdb::IndexedResults::check`] before deserializing statement `0` as JSON object rows.
#[inline]
pub(crate) fn check_and_take_zero<E>(
    response: surrealdb::IndexedResults,
    map_err: impl Fn(surrealdb::Error) -> E + Copy,
) -> std::result::Result<Vec<Value>, E> {
    let mut checked = response.check().map_err(map_err)?;
    checked.take(0).map_err(map_err)
}

pub(super) fn normalize_message_content(value: &Value) -> String {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

pub(super) fn is_empty_object(value: &Value) -> bool {
    matches!(value, Value::Object(m) if m.is_empty())
}

pub(super) fn has_meaningful_result(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}

/// Reserved for conversation_context tool metadata extraction.
#[allow(dead_code)]
pub(super) fn metadata_error(metadata: &Value) -> Option<Value> {
    let error = metadata.get("error")?;
    if has_meaningful_result(error) {
        Some(error.clone())
    } else {
        None
    }
}

pub(super) fn is_step_completed_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "done" | "step_completed" | "finished"
    )
}

pub(super) fn decode_depends_on(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .and_then(|value| value.as_array().cloned())
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Normalize payload text search query for SurrealDB BM25 full-text search.
pub(super) fn normalize_payload_text_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.replace('"', ""))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
