//! JSON serialization helpers.

use serde::Serialize;

use crate::{BamlRtError, Result};

/// Serializes a value to JSON. Maps serde_json errors to `BamlRtError::Json`.
pub fn to_json_value<T: Serialize>(value: &T) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(BamlRtError::Json)
}
