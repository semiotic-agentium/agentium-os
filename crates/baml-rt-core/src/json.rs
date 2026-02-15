//! JSON serialization helpers.

use serde::Serialize;

use crate::{BamlRtError, Result};

/// Serializes a value to JSON. Maps serde_json errors to `BamlRtError::Json`.
pub fn to_json_value<T: Serialize>(value: &T) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(BamlRtError::Json)
}

/// Returns true if the JSON chunk is a runtime tool-event marker.
pub fn is_a2a_tool_event_chunk(value: &serde_json::Value) -> bool {
    let Some(event) = value.get("event").and_then(|v| v.as_object()) else {
        return false;
    };
    let Some(source) = event.get("source").and_then(|v| v.as_str()) else {
        return false;
    };
    if source != "runtime" {
        return false;
    }
    let Some(event_type) = event.get("type").and_then(|v| v.as_str()) else {
        return false;
    };
    event_type.starts_with("tool_execution")
}
