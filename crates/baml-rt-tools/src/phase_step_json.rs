// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Normalize SessionPlan-shaped JSON into flat step objects for per-phase BAML unions.
//!
//! Host tool extraction accepts `{"step":{...}}` or flat `{"op":...}`; step-executor hops parse through
//! BAML into narrow unions that expect **flat** step shapes. Models often emit the wrapped form.

use serde_json::Value;

/// Extract a JSON value from raw LLM text (trimmed body, or first fenced ```json block).
pub fn extract_json_value_from_llm_text(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    let start = t.find("```")?;
    let after_fence = &t[start + 3..];
    let after_fence = after_fence
        .strip_prefix("json")
        .or_else(|| after_fence.strip_prefix("JSON"))
        .unwrap_or(after_fence)
        .trim_start();
    let end = after_fence.find("```")?;
    let inner = after_fence[..end].trim();
    serde_json::from_str(inner).ok()
}

/// If the model emitted umbrella `SessionPlan` JSON (`step` + optional top-level fields) but the
/// active BAML function expects a **flat** step union, promote `step` to the root object.
///
/// - When root has `op`, returns `v` unchanged.
/// - When root has `step` (object) with `op`, replaces the value with that object.
/// - If parent carried `citations` and inner is a Send step without `citations`, merges them.
pub fn unwrap_session_plan_step_shape_for_phase_output(v: Value) -> Value {
    let Value::Object(mut map) = v else {
        return v;
    };
    if map.contains_key("op") {
        return Value::Object(map);
    }
    let Some(step_val) = map.remove("step") else {
        return Value::Object(map);
    };
    let Value::Object(mut step_obj) = step_val else {
        map.insert("step".to_string(), step_val);
        return Value::Object(map);
    };
    if !step_obj.contains_key("op") {
        map.insert("step".to_string(), Value::Object(step_obj));
        return Value::Object(map);
    }
    let merge_cites = map.get("citations").cloned();
    if let Some(cites) = merge_cites {
        let is_send = step_obj
            .get("op")
            .and_then(|o| o.as_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("send"));
        if is_send && !step_obj.contains_key("citations") {
            step_obj.insert("citations".to_string(), cites);
        }
    }
    Value::Object(step_obj)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unwrap_promotes_step_to_root() {
        let v = json!({
            "step": { "op": "PageRead", "input": { "archive_ref": "@1", "offset": 0, "limit": 10 } },
            "citations": ["@1:L1-L3"]
        });
        let out = unwrap_session_plan_step_shape_for_phase_output(v);
        assert_eq!(
            out,
            json!({ "op": "PageRead", "input": { "archive_ref": "@1", "offset": 0, "limit": 10 } })
        );
    }

    #[test]
    fn unwrap_merges_citations_for_send_only() {
        let v = json!({
            "step": { "op": "Send", "input": { "text": "hi" } },
            "citations": ["#1"]
        });
        let out = unwrap_session_plan_step_shape_for_phase_output(v);
        assert_eq!(
            out,
            json!({ "op": "Send", "input": { "text": "hi" }, "citations": ["#1"] })
        );
    }

    #[test]
    fn flat_unchanged() {
        let v = json!({ "op": "Open", "tool_name": "support/crm" });
        let out = unwrap_session_plan_step_shape_for_phase_output(v.clone());
        assert_eq!(out, v);
    }
}
