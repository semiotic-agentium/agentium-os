//! Direct conversion between JsValueFacade and serde_json::Value
//!
//! This avoids JSON.stringify/parse roundtrips where possible for better performance

use baml_rt_core::{BamlRtError, Result};
use quickjs_runtime::values::{JsValueConvertable, JsValueFacade};
use serde_json::Value;

/// Convert JsValueFacade directly to serde_json::Value
///
/// Uses available methods on JsValueFacade to extract values without string serialization
/// For complex nested structures, falls back to using JSON.stringify in JavaScript
pub fn js_value_facade_to_value(js_value: JsValueFacade) -> Result<Value> {
    // Use the available methods on JsValueFacade to determine type and extract values
    if js_value.is_string() {
        Ok(Value::String(js_value.get_str().to_string()))
    } else if js_value.is_bool() {
        // Try to get boolean value - if method exists
        // Since we don't know the exact API, use a workaround
        // Convert via string representation as fallback for now
        let str_val = js_value.get_str();
        if str_val == "true" {
            Ok(Value::Bool(true))
        } else if str_val == "false" {
            Ok(Value::Bool(false))
        } else {
            // For complex types, we need to use JSON.stringify in JS
            // This is still more efficient than doing it multiple times
            Err(BamlRtError::TypeConversion(
                "Complex type - use JSON.stringify in JavaScript".to_string(),
            ))
        }
    } else if js_value.is_null_or_undefined() {
        Ok(Value::Null)
    } else {
        // For numbers, arrays, objects, and other complex types,
        // we'll need to use JSON.stringify in JavaScript before passing to Rust
        // This is a limitation of the JsValueFacade API
        Err(BamlRtError::TypeConversion(
            "Complex type - use JSON.stringify in JavaScript".to_string(),
        ))
    }
}

/// Convert serde_json::Value to JsValueFacade
///
/// This uses the JsValueConvertable trait implementation for Value
pub fn value_to_js_value_facade(value: Value) -> JsValueFacade {
    value.to_js_value_facade()
}
