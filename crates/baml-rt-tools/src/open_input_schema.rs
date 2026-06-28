// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON Schema predicates for tool `open_input`.
//!
//! Builder codegen (`initial_input` optional `?`) and runtime auto-open policy must agree
//! on when an empty or null-shaped open payload is allowed.

use serde_json::Value;

use crate::tools::{ToolCapability, ToolFunctionMetadata};

/// Whether the tool's `open_input` JSON Schema allows an empty object or null-shaped open
/// (builder: optional `initial_input`; runtime: strict auto-open before Send/Read).
pub fn schema_allows_empty_or_optional_open_input(schema: &Value) -> bool {
    match schema {
        Value::Null => true,
        Value::Object(map) => {
            // `JsonSchemaType for ()` and `serde_json::Value` use `{}` — unconstrained schema
            // that accepts any JSON including `{}`.
            if map.is_empty() {
                return true;
            }
            // `root_json_schema::<()>` adds only `$schema` (and similar) to the inline `{}`.
            let has_structural = map.contains_key("type")
                || map.contains_key("properties")
                || map.contains_key("required")
                || map.contains_key("anyOf")
                || map.contains_key("oneOf")
                || map.contains_key("allOf")
                || map.contains_key("enum");
            if !has_structural {
                return true;
            }
            if let Some(any_of) = map.get("anyOf").and_then(Value::as_array)
                && any_of
                    .iter()
                    .any(schema_allows_empty_or_optional_open_input)
            {
                return true;
            }
            if let Some(one_of) = map.get("oneOf").and_then(Value::as_array)
                && one_of
                    .iter()
                    .any(schema_allows_empty_or_optional_open_input)
            {
                return true;
            }
            if let Some(all_of) = map.get("allOf").and_then(Value::as_array)
                && !all_of.is_empty()
                && all_of
                    .iter()
                    .all(schema_allows_empty_or_optional_open_input)
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

/// Whether a tool may surface a typed `<Tool>SendStep` on the entry hop.
///
/// Requires one-shot semantics and an empty/optional open payload so the runtime can auto-open.
#[must_use]
pub fn entry_send_eligible(tool: &ToolFunctionMetadata) -> bool {
    tool.capability == ToolCapability::OneShot
        && schema_allows_empty_or_optional_open_input(&tool.open_input_schema)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_object_schema_allows() {
        assert!(schema_allows_empty_or_optional_open_input(&json!({})));
    }

    #[test]
    fn null_type_allows() {
        assert!(schema_allows_empty_or_optional_open_input(
            &json!({ "type": "null" })
        ));
    }

    #[test]
    fn object_with_required_disallows() {
        let schema = json!({
            "type": "object",
            "properties": { "k": { "type": "string" } },
            "required": ["k"]
        });
        assert!(!schema_allows_empty_or_optional_open_input(&schema));
    }

    fn sample_tool(open_input_schema: Value, capability: ToolCapability) -> ToolFunctionMetadata {
        ToolFunctionMetadata {
            name: crate::ToolName::parse("support/sample").expect("valid tool name"),
            class_name: "SupportSample".to_string(),
            description: "sample".to_string(),
            open_input_schema,
            input_schema: json!({}),
            output_schema: json!({}),
            open_input_type: crate::tools::ToolTypeSpec {
                name: "()".to_string(),
                ts_decl: None,
            },
            input_type: crate::tools::ToolTypeSpec {
                name: "SupportSampleInput".to_string(),
                ts_decl: None,
            },
            output_type: crate::tools::ToolTypeSpec {
                name: "SupportSampleOutput".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: Vec::new(),
            access: None,
            tags: Vec::new(),
            secret_requests: Vec::new(),
            config: None,
            config_bundle: None,
            origin: crate::ToolOrigin::Host,
            backend: crate::ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: crate::SessionPolicy::default(),
            capability,
            event_sources: Vec::new(),
            coordination_baml: None,
        }
    }

    #[test]
    fn entry_send_eligible_for_one_shot_empty_open() {
        let tool = sample_tool(json!({}), ToolCapability::OneShot);
        assert!(entry_send_eligible(&tool));
    }

    #[test]
    fn entry_send_ineligible_for_streaming() {
        let tool = sample_tool(json!({}), ToolCapability::Streaming);
        assert!(!entry_send_eligible(&tool));
    }

    #[test]
    fn entry_send_ineligible_for_required_open_input() {
        let schema = json!({
            "type": "object",
            "properties": { "k": { "type": "string" } },
            "required": ["k"]
        });
        let tool = sample_tool(schema, ToolCapability::OneShot);
        assert!(!entry_send_eligible(&tool));
    }
}
