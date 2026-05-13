//! Normalize an MCP `inputSchema` for snapshot storage.
//!
//! - Canonicalizes the schema (JCS) and computes a stable digest.
//! - Detects features the typed-BAML projection path cannot represent yet
//!   and flags the tool for `OpaqueJson` input fallback with an explicit
//!   reason so reviewers can see why typing was skipped.
//!
//! Actual JSON Schema → BAML lowering happens in the builder catalog
//! (later PR). This module's job is to decide whether the lowering should
//! be attempted at all and to produce a stable digest either way.

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::mcp_snapshot::Digest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSchema {
    pub schema: Value,
    pub digest: Digest,
    /// `Some(reason)` when the schema uses features the BAML lowering path
    /// cannot handle yet; the tool should fall back to `OpaqueJson` input.
    pub opaque_fallback_reason: Option<String>,
}

pub fn normalize(input: &Value) -> NormalizedSchema {
    let canonical_bytes = serde_jcs::to_vec(input).unwrap_or_else(|_| b"null".to_vec());
    let mut hasher = Sha256::new();
    hasher.update(&canonical_bytes);
    let digest = Digest::new(format!("sha256:{:x}", hasher.finalize()));

    let opaque_fallback_reason = first_unsupported_feature(input, &mut Vec::new());

    NormalizedSchema {
        schema: input.clone(),
        digest,
        opaque_fallback_reason,
    }
}

/// Returns the first reason the schema must fall back to `OpaqueJson`, or
/// `None` if the schema only uses currently-supportable features.
fn first_unsupported_feature(value: &Value, path: &mut Vec<String>) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(reason) = local_unsupported(map, path) {
                return Some(reason);
            }
            for (key, child) in map {
                if matches!(
                    key.as_str(),
                    "properties"
                        | "patternProperties"
                        | "items"
                        | "additionalProperties"
                        | "anyOf"
                        | "oneOf"
                        | "allOf"
                        | "not"
                        | "definitions"
                        | "$defs"
                ) {
                    path.push(key.clone());
                    let nested = recurse(child, path);
                    path.pop();
                    if nested.is_some() {
                        return nested;
                    }
                }
            }
            None
        }
        Value::Array(values) => {
            for (idx, child) in values.iter().enumerate() {
                path.push(format!("[{idx}]"));
                let nested = first_unsupported_feature(child, path);
                path.pop();
                if nested.is_some() {
                    return nested;
                }
            }
            None
        }
        _ => None,
    }
}

fn recurse(value: &Value, path: &mut Vec<String>) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                let nested = first_unsupported_feature(child, path);
                path.pop();
                if nested.is_some() {
                    return nested;
                }
            }
            None
        }
        Value::Array(values) => {
            for (idx, child) in values.iter().enumerate() {
                path.push(format!("[{idx}]"));
                let nested = first_unsupported_feature(child, path);
                path.pop();
                if nested.is_some() {
                    return nested;
                }
            }
            None
        }
        _ => None,
    }
}

fn local_unsupported(map: &Map<String, Value>, path: &[String]) -> Option<String> {
    let at = format_path(path);
    for keyword in [
        "$ref",
        "allOf",
        "not",
        "if",
        "then",
        "else",
        "dependentSchemas",
        "dependencies",
        "patternProperties",
    ] {
        if map.contains_key(keyword) {
            return Some(format!("unsupported keyword `{keyword}` at {at}"));
        }
    }
    if let Some(additional) = map.get("additionalProperties")
        && additional.is_object()
    {
        return Some(format!(
            "unsupported `additionalProperties` schema at {at}; only boolean is supported"
        ));
    }
    if let Some(union) = map.get("oneOf").or_else(|| map.get("anyOf"))
        && !is_simple_nullable_union(union)
    {
        return Some(format!(
            "unsupported complex `oneOf`/`anyOf` at {at}; only T | null patterns are typed"
        ));
    }
    None
}

fn is_simple_nullable_union(value: &Value) -> bool {
    let Some(arr) = value.as_array() else {
        return false;
    };
    if arr.len() != 2 {
        return false;
    }
    let mut has_null = false;
    let mut has_concrete = false;
    for member in arr {
        let Some(obj) = member.as_object() else {
            return false;
        };
        match obj.get("type") {
            Some(Value::String(t)) if t == "null" => has_null = true,
            Some(Value::String(_)) => has_concrete = true,
            _ => return false,
        }
    }
    has_null && has_concrete
}

fn format_path(path: &[String]) -> String {
    if path.is_empty() {
        "(root)".to_string()
    } else {
        format!("`{}`", path.join("."))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn typed_schema_is_supported() {
        let schema = json!({
            "type": "object",
            "properties": {
                "q": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["q"]
        });
        let normalized = normalize(&schema);
        assert!(normalized.opaque_fallback_reason.is_none());
        assert!(normalized.digest.as_str().starts_with("sha256:"));
    }

    #[test]
    fn ref_triggers_fallback() {
        let schema = json!({
            "type": "object",
            "properties": {
                "child": { "$ref": "#/definitions/Foo" }
            }
        });
        let normalized = normalize(&schema);
        let reason = normalized.opaque_fallback_reason.expect("fallback");
        assert!(reason.contains("$ref"));
    }

    #[test]
    fn all_of_triggers_fallback() {
        let schema = json!({
            "allOf": [
                { "type": "object" },
                { "type": "object" }
            ]
        });
        let reason = normalize(&schema).opaque_fallback_reason.expect("fallback");
        assert!(reason.contains("allOf"));
    }

    #[test]
    fn pattern_properties_triggers_fallback() {
        let schema = json!({
            "type": "object",
            "patternProperties": { "^x-": { "type": "string" } }
        });
        let reason = normalize(&schema).opaque_fallback_reason.expect("fallback");
        assert!(reason.contains("patternProperties"));
    }

    #[test]
    fn additional_properties_schema_triggers_fallback() {
        let schema = json!({
            "type": "object",
            "additionalProperties": { "type": "string" }
        });
        let reason = normalize(&schema).opaque_fallback_reason.expect("fallback");
        assert!(reason.contains("additionalProperties"));
    }

    #[test]
    fn additional_properties_boolean_is_supported() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "a": { "type": "string" } }
        });
        assert!(normalize(&schema).opaque_fallback_reason.is_none());
    }

    #[test]
    fn nullable_oneof_is_supported() {
        let schema = json!({
            "type": "object",
            "properties": {
                "v": { "oneOf": [{ "type": "string" }, { "type": "null" }] }
            }
        });
        assert!(normalize(&schema).opaque_fallback_reason.is_none());
    }

    #[test]
    fn complex_oneof_triggers_fallback() {
        let schema = json!({
            "type": "object",
            "properties": {
                "v": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "integer" }
                    ]
                }
            }
        });
        let reason = normalize(&schema).opaque_fallback_reason.expect("fallback");
        assert!(reason.contains("oneOf"));
    }

    #[test]
    fn nested_unsupported_keyword_is_detected() {
        let schema = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {
                        "inner": { "$ref": "#/Foo" }
                    }
                }
            }
        });
        let reason = normalize(&schema).opaque_fallback_reason.expect("fallback");
        assert!(reason.contains("$ref"));
        assert!(reason.contains("properties"));
    }

    #[test]
    fn digest_is_stable_across_key_order() {
        let a = json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "integer" } }
        });
        let b = json!({
            "properties": { "b": { "type": "integer" }, "a": { "type": "string" } },
            "type": "object"
        });
        assert_eq!(normalize(&a).digest, normalize(&b).digest);
    }
}
