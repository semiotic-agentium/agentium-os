//! Tool call and tool session plan extraction from JSON.
//!
//! Session plans are bound to a tool by manifest mapping (function name -> plan type),
//! then resolved from the registry via `ToolFunctionMetadata::class_name`.
//! Single tool calls still resolve by input schema when exactly one tool matches.

use baml_rt_core::{BamlRtError, Result, StepPlanRecovery};
use baml_rt_tools::{SessionPlanTypeName, ToolName, ToolRegistry as ConcreteToolRegistry};
use serde_json::Value;

/// Resolve tool name from a known session plan type name (e.g. from the builder-generated manifest).
///
/// Used when the invoking BAML function is known and we have a manifest mapping function → plan type,
/// so we do not rely on __type in the prompt output.
pub(crate) fn resolve_tool_name_from_plan_type_with_registry(
    registry: &ConcreteToolRegistry,
    plan_type: &SessionPlanTypeName,
) -> Result<ToolName> {
    let class_name = plan_type.class_name();

    let matches: Vec<ToolName> = registry
        .all_metadata()
        .into_iter()
        .filter(|metadata| metadata.class_name == class_name)
        .map(|metadata| metadata.name.clone())
        .collect();

    match matches.len() {
        // SAFETY: len == 1 verified by match guard above.
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => Err(BamlRtError::InvalidArgument(format!(
            "No registered tool has class_name {class_name:?} (from plan type {}). Ensure the tool is registered.",
            plan_type.as_str()
        ))),
        _ => {
            let names: Vec<String> = matches.iter().map(|n| n.to_string()).collect();
            Err(BamlRtError::InvalidArgument(format!(
                "Multiple tools match session plan class {class_name:?}: {}.",
                names.join(", ")
            )))
        }
    }
}

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
) -> Result<ToolName> {
    let mut matches: Vec<ToolName> = registry
        .all_metadata()
        .into_iter()
        .filter_map(|metadata| {
            if input_matches_schema(input, &metadata.input_schema) {
                Some(metadata.name.clone())
            } else {
                None
            }
        })
        .collect();
    match matches.len() {
        // SAFETY: len == 1 verified by match guard above.
        1 => Ok(matches.pop().unwrap()),
        0 => Err(BamlRtError::InvalidArgument(format!(
            "No tool input schema matched input: {input}"
        ))),
        _ => {
            let names: Vec<String> = matches.iter().map(|n| n.to_string()).collect();
            Err(BamlRtError::InvalidArgument(format!(
                "Multiple tools matched input schema: {}",
                names.join(", ")
            )))
        }
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
            if let Some(req_key) = req.as_str()
                && !input_obj.contains_key(req_key)
            {
                return false;
            }
        }
    }
    true
}

/// Typed tool session operation (replaces stringly-typed `op` field).
///
/// Encodes FSM operations at compile time for type-safe plan execution.
/// `reason` is the optional LLM-supplied explanation for the step; used in tracing and for Abort passed to `tool_session_abort`.
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
    /// Line-filtered archive read; `grep` pattern is required and must parse.
    SearchRead {
        archive_ref: baml_rt_tools::archive_read::ShortRef,
        offset: baml_rt_tools::archive_read::LineOffset,
        limit: baml_rt_tools::archive_read::PageLimit,
        grep: baml_rt_tools::archive_read::GrepPattern,
        reason: Option<String>,
    },
    /// Contiguous archive paging without a line filter.
    PageRead {
        archive_ref: baml_rt_tools::archive_read::ShortRef,
        offset: baml_rt_tools::archive_read::LineOffset,
        limit: baml_rt_tools::archive_read::PageLimit,
        reason: Option<String>,
    },
    Finish {
        reason: Option<String>,
    },
    Abort {
        reason: Option<String>,
    },
}

impl ToolSessionOp {
    /// FSM op label for tracing and plan coercion (`Open`, `Send`, …).
    #[allow(dead_code)] // Used by upcoming trace / coercion paths; kept when `baml` is split by module.
    pub(crate) fn op_name(&self) -> &'static str {
        match self {
            ToolSessionOp::Open { .. } => "Open",
            ToolSessionOp::Send { .. } => "Send",
            ToolSessionOp::SearchRead { .. } => "SearchRead",
            ToolSessionOp::PageRead { .. } => "PageRead",
            ToolSessionOp::Finish { .. } => "Finish",
            ToolSessionOp::Abort { .. } => "Abort",
        }
    }
}

/// Plan-level result of extracting a single tool session fragment plus optional plan reason.
#[derive(Debug, Clone)]
pub(crate) struct ToolSessionPlan {
    pub step: ToolSessionOp,
    /// Explicit tool selection for polymorphic Open.
    /// Set when the Open step contains a `tool_name` field, validated as a `ToolName`.
    /// `None` for single-tool functions where tool identity is bound by plan type.
    pub selected_tool: Option<ToolName>,
}

impl ToolSessionPlan {
    pub(crate) fn is_archive_read(&self) -> bool {
        matches!(
            self.step,
            ToolSessionOp::SearchRead { .. } | ToolSessionOp::PageRead { .. }
        )
    }
}

/// Extract and convert a JSON tool session fragment into one typed operation and optional plan-level reason.
pub(crate) fn extract_tool_session_plan(result: &Value) -> Result<Option<ToolSessionPlan>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };
    // Support both wrapped `{"step": {"op": ...}}` and flat `{"op": ...}` step objects.
    // Flat form is produced by per-phase functions that return bare step types
    // (e.g. SupportCrmOpenStep) without a SessionPlan wrapper.
    let step_obj = if let Some(step_value) = obj.get("step") {
        step_value.as_object().ok_or_else(|| {
            BamlRtError::from(StepPlanRecovery::tool_session_plan_step_not_object())
        })?
    } else if obj.contains_key("op") {
        obj
    } else {
        return Ok(None);
    };
    // Extract optional tool_name for polymorphic Open (tool selection).
    // Parsed as ToolName to validate format ("bundle/local"). For single-tool
    // functions the caller validates that this is None.
    let selected_tool = step_obj
        .get("tool_name")
        .and_then(Value::as_str)
        .map(ToolName::parse)
        .transpose()?;

    let op_str = match step_obj.get("op").and_then(|v| v.as_str()) {
        Some(s) => s.to_ascii_lowercase(),
        None => {
            tracing::warn!(raw_step = ?step_obj, "ToolSessionPlan step missing op");
            return Err(BamlRtError::from(
                StepPlanRecovery::tool_session_step_missing_op(),
            ));
        }
    };

    // Read-only terminal: no host tool session — not an FSM extractable plan.
    if op_str == "readonlyfinish" {
        return Ok(None);
    }

    let reason = step_obj
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let step = match op_str.as_str() {
        "open" => {
            let initial_input = step_obj.get("initial_input").cloned();
            ToolSessionOp::Open {
                initial_input,
                reason,
            }
        }
        "send" => {
            let input = step_obj
                .get("input")
                .cloned()
                .ok_or_else(|| BamlRtError::from(StepPlanRecovery::send_missing_input()))
                .and_then(|v| {
                    if v.is_null() {
                        Err(BamlRtError::from(StepPlanRecovery::send_input_null()))
                    } else {
                        Ok(v)
                    }
                })?;
            ToolSessionOp::Send { input, reason }
        }
        "read" => {
            return Err(BamlRtError::from(StepPlanRecovery::legacy_read_op_removed()));
        }
        "searchread" => {
            let input = step_obj
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            let archive_ref = input
                .get("archive_ref")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .and_then(baml_rt_tools::archive_read::ShortRef::parse)
                .ok_or_else(|| {
                    BamlRtError::from(StepPlanRecovery::missing_archive_ref_search_read())
                })?;
            let offset = input
                .get("offset")
                .and_then(|v| v.as_u64())
                .map(|n| baml_rt_tools::archive_read::LineOffset(n as usize))
                .unwrap_or_default();
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| baml_rt_tools::archive_read::PageLimit::new(n as usize))
                .unwrap_or_default();
            let grep_raw = input
                .get("grep")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| BamlRtError::from(StepPlanRecovery::missing_search_read_grep()))?;
            let grep = baml_rt_tools::archive_read::GrepPattern::parse(grep_raw).map_err(|e| {
                BamlRtError::from(StepPlanRecovery::invalid_search_read_grep(
                    grep_raw,
                    &e.to_string(),
                ))
            })?;
            ToolSessionOp::SearchRead {
                archive_ref,
                offset,
                limit,
                grep,
                reason,
            }
        }
        "pageread" => {
            let input = step_obj
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            if let Some(g) = input.get("grep").and_then(|v| v.as_str())
                && !g.is_empty()
            {
                return Err(BamlRtError::from(
                    StepPlanRecovery::page_read_grep_forbidden(),
                ));
            }
            let archive_ref = input
                .get("archive_ref")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .and_then(baml_rt_tools::archive_read::ShortRef::parse)
                .ok_or_else(|| {
                    BamlRtError::from(StepPlanRecovery::missing_archive_ref_page_read())
                })?;
            let offset = input
                .get("offset")
                .and_then(|v| v.as_u64())
                .map(|n| baml_rt_tools::archive_read::LineOffset(n as usize))
                .unwrap_or_default();
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| baml_rt_tools::archive_read::PageLimit::new(n as usize))
                .unwrap_or_default();
            ToolSessionOp::PageRead {
                archive_ref,
                offset,
                limit,
                reason,
            }
        }
        "finish" => ToolSessionOp::Finish { reason },
        "abort" => ToolSessionOp::Abort { reason },
        other => {
            return Err(BamlRtError::from(
                StepPlanRecovery::unknown_tool_session_op(other),
            ));
        }
    };

    Ok(Some(ToolSessionPlan {
        step,
        selected_tool,
    }))
}

/// Normalize plan input (string JSON → parsed Value).
pub(crate) fn normalize_plan_input(value: Value) -> Result<Value> {
    match value {
        Value::String(raw) => serde_json::from_str(&raw).map_err(|e| {
            BamlRtError::from(StepPlanRecovery::invalid_plan_input_json(&e.to_string()))
        }),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn proptest_cfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(cases);
        cfg.failure_persistence = None;
        cfg
    }

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
        #![proptest_config(proptest_cfg(64))]

        /// Invariant: When extraction returns Some(ToolCall), args never contain "tool_name".
        #[test]
        fn prop_extract_tool_call_args_never_contain_tool_name(v in prop_oneof![valid_tool_call_object(), valid_single_key_wrapper()]) {
            let res = extract_tool_call(&v).unwrap();
            if let Some(call) = res {
                let obj = call.args.as_object();
                assert!(
                    !obj.is_some_and(|m| m.contains_key("tool_name")),
                    "extract_tool_call must not expose tool_name in args"
                );
            }
        }
    }

    proptest! {
        #![proptest_config(proptest_cfg(32))]

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

    /// Single step object with valid op values, no tool_name.
    fn valid_step_object() -> impl Strategy<Value = Value> {
        let op_strategy = prop_oneof![Just("open"), Just("finish"), Just("abort"),];
        op_strategy.prop_map(|op| {
            let mut step = serde_json::Map::new();
            step.insert("op".to_string(), Value::String(op.to_string()));
            step.insert("__type".to_string(), Value::String("Step".to_string()));
            let mut obj = serde_json::Map::new();
            obj.insert("step".to_string(), Value::Object(step));
            Value::Object(obj)
        })
    }

    proptest! {
        #![proptest_config(proptest_cfg(32))]

        /// Invariant: Extracted plan steps have valid op and no tool_name in payload.
        #[test]
        fn prop_extract_plan_step_valid(v in valid_step_object()) {
            let plan = extract_tool_session_plan(&v).unwrap().expect("plan");
            assert!(plan.selected_tool.is_none(), "steps without tool_name must have selected_tool=None");
            match plan.step {
                ToolSessionOp::Open { .. }
                | ToolSessionOp::Send { .. }
                | ToolSessionOp::SearchRead { .. }
                | ToolSessionOp::PageRead { .. }
                | ToolSessionOp::Finish { .. }
                | ToolSessionOp::Abort { .. } => {}
            }
        }
    }

    #[test]
    fn extract_tool_session_plan_polymorphic_open_parses_selected_tool() {
        let json: Value = serde_json::json!({
            "step": {
                "op": "Open",
                "tool_name": "support/calculate",
                "initial_input": { "expression": "2 + 3" }
            }
        });
        let plan = extract_tool_session_plan(&json)
            .unwrap()
            .expect("should extract plan");
        assert_eq!(
            plan.selected_tool.as_ref().map(|t| t.to_string()),
            Some("support/calculate".to_string())
        );
        match plan.step {
            ToolSessionOp::Open { initial_input, .. } => {
                assert!(initial_input.is_some());
            }
            _ => panic!("expected Open step"),
        }
    }

    #[test]
    fn extract_tool_session_plan_open_without_tool_name_has_none() {
        let json: Value = serde_json::json!({
            "step": { "op": "Open" }
        });
        let plan = extract_tool_session_plan(&json)
            .unwrap()
            .expect("should extract plan");
        assert!(plan.selected_tool.is_none());
    }

    #[test]
    fn extract_tool_session_plan_invalid_tool_name_format_rejected() {
        let json: Value = serde_json::json!({
            "step": {
                "op": "Open",
                "tool_name": "no_slash_here"
            }
        });
        let result = extract_tool_session_plan(&json);
        assert!(result.is_err(), "invalid tool_name format must be rejected");
    }

    #[test]
    fn extract_tool_session_plan_read_only_finish_not_a_session_plan() {
        let json: Value = serde_json::json!({
            "op": "ReadOnlyFinish",
            "reply": { "parts": [{"type": "text", "text": "from archive"}], "citations": ["@1"] }
        });
        assert!(extract_tool_session_plan(&json).unwrap().is_none());
    }

    #[test]
    fn extract_tool_session_plan_send_step_selected_tool_is_none() {
        let json: Value = serde_json::json!({
            "step": {
                "op": "Send",
                "input": { "expression": "7 * 8" }
            }
        });
        let plan = extract_tool_session_plan(&json)
            .unwrap()
            .expect("should extract plan");
        assert!(plan.selected_tool.is_none());
        match plan.step {
            ToolSessionOp::Send { input, .. } => {
                assert_eq!(input, serde_json::json!({ "expression": "7 * 8" }));
            }
            _ => panic!("expected Send step"),
        }
    }

    fn polymorphic_open_step(tool_name: &str) -> impl Strategy<Value = Value> {
        let tn = tool_name.to_string();
        Just(()).prop_map(move |_| {
            serde_json::json!({
                "step": {
                    "op": "Open",
                    "tool_name": tn,
                    "__type": "SomeStep"
                }
            })
        })
    }

    proptest! {
        #![proptest_config(proptest_cfg(16))]

        /// Invariant: polymorphic Open steps with valid tool_name always produce Some(selected_tool).
        #[test]
        fn prop_polymorphic_open_preserves_selected_tool(
            v in prop_oneof![
                polymorphic_open_step("support/calculate"),
                polymorphic_open_step("system/internal_a2a"),
                polymorphic_open_step("claude/dev"),
            ]
        ) {
            let plan = extract_tool_session_plan(&v).unwrap().expect("plan");
            assert!(plan.selected_tool.is_some(), "polymorphic Open must have selected_tool");
            matches!(plan.step, ToolSessionOp::Open { .. });
        }
    }
}
