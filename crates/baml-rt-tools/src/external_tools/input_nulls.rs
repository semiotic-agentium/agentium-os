// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Map BAML's "absent optional" (`null`) onto JSON Schema's "absent optional"
//! (key omitted) at the external-tool boundary.
//!
//! BAML renders an optional tool field as `T?`, so the model emits `null` for
//! fields it leaves unset. MCP tool schemas express optionality by leaving the
//! property out of `required`, with a non-nullable `type` (e.g. zod
//! `.optional()` → `{"type": "string"}`). Forwarding the `null` makes the tool
//! reject the call ("Expected string, received null").
//!
//! The rewrite is deliberately narrow: a key is dropped only when its value is
//! `null`, it is not `required`, and its property schema does not itself admit
//! `null`. Required or nullable fields, and anything the schema does not
//! describe, are forwarded untouched so the tool still validates them.

use serde_json::{Map, Value};

/// Remove `null` values the input schema cannot accept for optional fields.
pub fn omit_null_optional_fields(schema: &Value, input: Value) -> Value {
    match input {
        Value::Object(map) => Value::Object(omit_in_object(schema, map)),
        Value::Array(items) => match schema.get("items") {
            Some(item_schema) if item_schema.is_object() => Value::Array(
                items
                    .into_iter()
                    .map(|item| omit_null_optional_fields(item_schema, item))
                    .collect(),
            ),
            _ => Value::Array(items),
        },
        other => other,
    }
}

fn omit_in_object(schema: &Value, map: Map<String, Value>) -> Map<String, Value> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return map;
    };
    let required = |key: &str| {
        schema
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|names| names.iter().any(|name| name.as_str() == Some(key)))
    };
    map.into_iter()
        .filter_map(|(key, value)| match properties.get(&key) {
            Some(property) if value.is_null() => {
                (required(&key) || admits_null(property)).then_some((key, value))
            }
            Some(property) => {
                let value = omit_null_optional_fields(property, value);
                Some((key, value))
            }
            None => Some((key, value)),
        })
        .collect()
}

/// Whether a property schema explicitly accepts `null`. Unknown or untyped
/// schemas (`{}`, `true`) accept anything, so they keep the `null`.
fn admits_null(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        // Boolean schema `true` accepts null; `false` accepts nothing, and
        // dropping an optional key is then the only valid choice.
        return schema.as_bool().unwrap_or(false);
    };
    if object.get("nullable").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if object.get("const").is_some_and(Value::is_null) {
        return true;
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return values.iter().any(Value::is_null);
    }
    for combinator in ["anyOf", "oneOf"] {
        if let Some(branches) = object.get(combinator).and_then(Value::as_array) {
            return branches.iter().any(admits_null);
        }
    }
    match object.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("null")),
        // No type constraint (and no $ref we can resolve here): keep the value
        // and let the tool's own validation decide.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn meteo_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "location_query": {"type": "string"},
                "city": {"type": "string"},
                "country": {"type": "string"},
                "timezone": {"type": "string", "default": "auto"},
                "hourly_limit": {"type": "integer", "minimum": 1, "maximum": 48},
            },
            "required": ["location_query"],
            "additionalProperties": false,
        })
    }

    #[test]
    fn drops_nulls_for_optional_non_nullable_fields() {
        let input = json!({
            "location_query": "Paris, France",
            "city": null, "country": null, "timezone": null, "hourly_limit": null,
        });
        assert_eq!(
            omit_null_optional_fields(&meteo_schema(), input),
            json!({"location_query": "Paris, France"})
        );
    }

    #[test]
    fn keeps_null_for_required_fields_so_the_tool_rejects_it() {
        let input = json!({"location_query": null});
        assert_eq!(
            omit_null_optional_fields(&meteo_schema(), input.clone()),
            input
        );
    }

    #[test]
    fn keeps_null_when_the_schema_admits_it() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": ["string", "null"]},
                "b": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                "c": {"type": "string", "nullable": true},
                "d": {},
            },
        });
        let input = json!({"a": null, "b": null, "c": null, "d": null});
        assert_eq!(omit_null_optional_fields(&schema, input.clone()), input);
    }

    #[test]
    fn recurses_into_nested_objects_and_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "object",
                    "properties": {"tag": {"type": "string"}, "limit": {"type": "integer"}},
                },
                "points": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"label": {"type": "string"}},
                    },
                },
            },
        });
        let input = json!({
            "filter": {"tag": null, "limit": 3},
            "points": [{"label": null}, {"label": "x"}],
        });
        assert_eq!(
            omit_null_optional_fields(&schema, input),
            json!({"filter": {"limit": 3}, "points": [{}, {"label": "x"}]})
        );
    }

    #[test]
    fn leaves_input_alone_without_a_described_schema() {
        let input = json!({"anything": null});
        assert_eq!(omit_null_optional_fields(&json!({}), input.clone()), input);
        assert_eq!(
            omit_null_optional_fields(&json!({"type": "object"}), input.clone()),
            input
        );
    }
}
