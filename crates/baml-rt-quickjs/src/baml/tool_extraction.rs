//! Tool call and tool session plan extraction from JSON.
//!
//! Parses BAML tool call results and `ToolSessionPlan` steps into typed
//! structures for execution. Tool identity is derived from input schema
//! matching; no raw `tool_name` in payload for attribution.

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::ToolRegistry as ConcreteToolRegistry;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ToolCall {
    pub args: Value,
}

/// Extract a single tool call from a BAML result if present.
pub(crate) fn extract_tool_call(result: &Value) -> Result<Option<ToolCall>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };

    if obj.contains_key("tool_name") {
        return Err(BamlRtError::InvalidArgument(
            "Tool call must not include tool_name; tool identity is derived from input schema"
                .to_string(),
        ));
    }

    if obj.get("__type").is_some() {
        let mut tool_args = serde_json::Map::new();
        for (key, value) in obj {
            if key != "__type" {
                tool_args.insert(key.clone(), value.clone());
            }
        }
        return Ok(Some(ToolCall {
            args: Value::Object(tool_args),
        }));
    }

    if obj.len() == 1 {
        let (_, value) = obj.iter().next().ok_or_else(|| {
            BamlRtError::InvalidArgument("Expected non-empty tool object".to_string())
        })?;
        if let Some(inner) = value.as_object() {
            if inner.contains_key("tool_name") {
                return Err(BamlRtError::InvalidArgument(
                    "Tool call must not include tool_name; tool identity is derived from input schema"
                        .to_string(),
                ));
            }
            let mut tool_args = serde_json::Map::new();
            for (key, value) in inner {
                if key != "__type" {
                    tool_args.insert(key.clone(), value.clone());
                }
            }
            return Ok(Some(ToolCall {
                args: Value::Object(tool_args),
            }));
        }
    }

    Ok(None)
}

/// Resolve tool name from input by matching against registry metadata.
pub(crate) fn resolve_tool_name_from_input_with_registry(
    registry: &ConcreteToolRegistry,
    input: &Value,
) -> Result<String> {
    let mut matches = registry
        .all_metadata()
        .into_iter()
        .filter_map(|metadata| {
            if input_matches_schema(input, &metadata.input_schema) {
                Some(metadata.name.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    match matches.len() {
        1 => Ok(matches.pop().unwrap()),
        0 => Err(BamlRtError::InvalidArgument(format!(
            "No tool input schema matched input: {}",
            input
        ))),
        _ => Err(BamlRtError::InvalidArgument(format!(
            "Multiple tools matched input schema: {}",
            matches.join(", ")
        ))),
    }
}

fn input_matches_schema(input: &Value, schema: &Value) -> bool {
    let input_obj = match input.as_object() {
        Some(obj) => obj,
        None => return false,
    };
    let schema_obj = match schema.as_object() {
        Some(obj) => obj,
        None => return false,
    };
    if let Some(Value::String(schema_type)) = schema_obj.get("type")
        && schema_type != "object"
    {
        return false;
    }
    if let Some(required) = schema_obj.get("required").and_then(|v| v.as_array()) {
        for req in required {
            if let Some(req_key) = req.as_str() {
                if !input_obj.contains_key(req_key) {
                    return false;
                }
            }
        }
    }
    true
}

/// Typed tool session operation (replaces stringly-typed `op` field).
///
/// Encodes FSM operations at compile time for type-safe plan execution.
#[derive(Debug, Clone)]
pub enum ToolSessionOp {
    Open {
        initial_input: Option<Value>,
        reason: Option<String>,
    },
    Send {
        input: Value,
        reason: Option<String>,
    },
    Next {
        reason: Option<String>,
    },
    Finish {
        reason: Option<String>,
    },
    Abort {
        reason: Option<String>,
    },
}

/// Extract and convert JSON tool session plan into typed operations.
pub(crate) fn extract_tool_session_plan(result: &Value) -> Result<Option<Vec<ToolSessionOp>>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };
    let steps_value = match obj.get("steps") {
        Some(value) => value,
        None => return Ok(None),
    };
    let steps_array = steps_value.as_array().ok_or_else(|| {
        BamlRtError::InvalidArgument("ToolSessionPlan.steps must be an array".to_string())
    })?;

    let mut steps = Vec::new();
    for step_value in steps_array {
        let step_obj = step_value.as_object().ok_or_else(|| {
            BamlRtError::InvalidArgument("ToolSessionPlan step must be an object".to_string())
        })?;
        if step_obj.contains_key("tool_name") {
            return Err(BamlRtError::InvalidArgument(
                "ToolSessionPlan step must not include tool_name; tool identity is bound by plan type"
                    .to_string(),
            ));
        }

        let step_type = step_obj.get("__type").and_then(|v| v.as_str());
        let op_str = step_obj
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("ToolSessionPlan step missing op".to_string())
            })?
            .to_ascii_lowercase();

        let reason = step_obj
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let op = match op_str.as_str() {
            "open" => {
                let initial_input = match step_type {
                    Some("SupportCalculateOpenStep") | Some(_) => {
                        step_obj.get("initial_input").cloned()
                    }
                    _ => step_obj.get("initial_input").cloned(),
                };
                ToolSessionOp::Open {
                    initial_input,
                    reason,
                }
            }
            "send" => {
                let input = match step_type {
                    Some("SupportCalculateSendStep") | Some(_) => {
                        step_obj.get("input").cloned().ok_or_else(|| {
                            BamlRtError::InvalidArgument(
                                "Send step missing required 'input' field".to_string(),
                            )
                        })?
                    }
                    _ => step_obj.get("input").cloned().ok_or_else(|| {
                        BamlRtError::InvalidArgument(
                            "Send step missing required 'input' field".to_string(),
                        )
                    })?,
                };
                ToolSessionOp::Send { input, reason }
            }
            "next" => ToolSessionOp::Next { reason },
            "finish" => ToolSessionOp::Finish { reason },
            "abort" => ToolSessionOp::Abort { reason },
            other => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Unknown tool session op '{}'",
                    other
                )));
            }
        };

        steps.push(op);
    }

    Ok(Some(steps))
}

/// Normalize plan input (string JSON → parsed Value).
pub(crate) fn normalize_plan_input(value: Value) -> Result<Value> {
    match value {
        Value::String(raw) => serde_json::from_str(&raw)
            .map_err(|e| BamlRtError::InvalidArgument(format!("Invalid plan input JSON: {}", e))),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Valid tool-call object: has __type, no tool_name. Optional extra keys (reserved excluded).
    fn valid_tool_call_object() -> impl Strategy<Value = Value> {
        let extra_keys = ["a", "b", "x", "reason", "input"];
        let extra = prop::collection::vec(
            (0..extra_keys.len()).prop_map(move |i| extra_keys[i].to_string()),
            0..4,
        )
        .prop_map(|keys| {
            let mut m = serde_json::Map::new();
            for (i, k) in keys.into_iter().enumerate() {
                m.insert(k, Value::Number(serde_json::Number::from(i as i64)));
            }
            m
        });
        extra.prop_map(|mut m| {
            m.insert("__type".to_string(), Value::String("SomeType".to_string()));
            Value::Object(m)
        })
    }

    /// Single-key wrapper: { "ToolName": { __type, ... } } with no tool_name.
    fn valid_single_key_wrapper() -> impl Strategy<Value = Value> {
        valid_tool_call_object().prop_map(|inner| {
            let mut outer = serde_json::Map::new();
            outer.insert("T".to_string(), inner);
            Value::Object(outer)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Invariant: When extraction returns Some(ToolCall), args never contain "tool_name".
        #[test]
        fn prop_extract_tool_call_args_never_contain_tool_name(v in prop_oneof![valid_tool_call_object(), valid_single_key_wrapper()]) {
            let res = extract_tool_call(&v).unwrap();
            if let Some(call) = res {
                let obj = call.args.as_object();
                assert!(
                    obj.map_or(true, |m| !m.contains_key("tool_name")),
                    "extract_tool_call must not expose tool_name in args"
                );
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Invariant: normalize_plan_input(Value::String(json)) == parse(json) for valid JSON.
        #[test]
        fn prop_normalize_plan_input_string_roundtrip(v in prop::collection::vec(any::<i64>(), 0..8)) {
            let value = Value::Array(
                v.into_iter()
                    .map(|n| Value::Number(serde_json::Number::from(n)))
                    .collect::<Vec<_>>(),
            );
            let json = serde_json::to_string(&value).unwrap();
            let normalized = normalize_plan_input(Value::String(json)).unwrap();
            assert_eq!(normalized, value, "normalize_plan_input roundtrip");
        }
    }

    /// Steps array with valid op values, no tool_name.
    fn valid_steps_array() -> impl Strategy<Value = Value> {
        let op_strategy = prop_oneof![Just("open"), Just("next"), Just("finish"), Just("abort"),];
        prop::collection::vec(
            op_strategy.prop_map(|op| {
                let mut step = serde_json::Map::new();
                step.insert("op".to_string(), Value::String(op.to_string()));
                step.insert("__type".to_string(), Value::String("Step".to_string()));
                Value::Object(step)
            }),
            1..6,
        )
        .prop_map(|steps| {
            let mut obj = serde_json::Map::new();
            obj.insert("steps".to_string(), Value::Array(steps));
            Value::Object(obj)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Invariant: Extracted plan steps have valid op and no tool_name in payload.
        #[test]
        fn prop_extract_plan_steps_valid(v in valid_steps_array()) {
            let steps = extract_tool_session_plan(&v).unwrap().expect("steps");
            assert!(!steps.is_empty());
            // All steps were parsed; type system enforces ToolSessionOp variants (no tool_name).
            assert_eq!(steps.len(), v.get("steps").and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0));
        }
    }
}
