//! Recover session-plan JSON from raw LLM text when BAML jsonish enum matching fails.
//!
//! Models sometimes nest objects where scalar enums belong (e.g. calculator `operation`),
//! which stringifies to ambiguous substrings and triggers "Too many matches for MathOperation".

use serde_json::Value;

/// Extract the outermost JSON object from LLM text and normalize known scalar-enum pitfalls.
#[must_use]
pub(crate) fn try_salvage_json_from_llm_content(content: &str) -> Option<Value> {
    let extracted = extract_outer_json_object(content)?;
    let mut value: Value = serde_json::from_str(&extracted).ok()?;
    normalize_calculator_operation_fields(&mut value);
    Some(value)
}

fn extract_outer_json_object(content: &str) -> Option<String> {
    let start = content.find('{')?;
    let slice = &content[start..];
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in slice.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(slice[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn normalize_calculator_operation_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(op) = map.get_mut("operation")
                && let Some(norm) = normalize_math_operation_value(op)
            {
                *op = norm;
            }
            for v in map.values_mut() {
                normalize_calculator_operation_fields(v);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                normalize_calculator_operation_fields(v);
            }
        }
        _ => {}
    }
}

fn normalize_math_operation_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => normalize_math_operation_string(s).map(Value::String),
        Value::Object(map) => {
            for key in ["operation", "op", "type", "value", "name"] {
                if let Some(inner) = map.get(key)
                    && let Some(norm) = normalize_math_operation_value(inner)
                {
                    return Some(norm);
                }
            }
            None
        }
        _ => None,
    }
}

fn normalize_math_operation_string(raw: &str) -> Option<String> {
    let t = raw.trim();
    const EXACT: &[(&str, &str)] = &[
        ("+", "+"),
        ("-", "-"),
        ("*", "*"),
        ("/", "/"),
        ("Add", "Add"),
        ("Subtract", "Subtract"),
        ("Multiply", "Multiply"),
        ("Divide", "Divide"),
    ];
    for (from, to) in EXACT {
        if t == *from {
            return Some((*to).to_string());
        }
    }
    // Substring recovery when jsonish stringified a nested object.
    let lower = t.to_lowercase();
    let mut hits: Vec<&str> = Vec::new();
    for (needle, token) in [
        ("add", "Add"),
        ("subtract", "Subtract"),
        ("multiply", "Multiply"),
        ("divide", "Divide"),
        ("+", "+"),
        ("-", "-"),
        ("*", "*"),
        ("/", "/"),
    ] {
        if lower.contains(needle) {
            hits.push(token);
        }
    }
    hits.sort();
    hits.dedup();
    match hits.as_slice() {
        [one] => Some((*one).to_string()),
        [] => None,
        _ => {
            // Prefer symbolic when both name and symbol appear (common nested-object artifact).
            for sym in ["+", "-", "*", "/"] {
                if hits.contains(&sym) {
                    return Some(sym.to_string());
                }
            }
            hits.first().map(|s| (*s).to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salvage_nested_operation_object() {
        let content = r#"Here is the step:
{"op":"Send","step":{"input":{"expression":{"left":1,"operation":{"operation":"Add"},"right":2}}}}}"#;
        let v = try_salvage_json_from_llm_content(content).expect("salvage");
        assert_eq!(v["step"]["input"]["expression"]["operation"], "Add");
    }

    #[test]
    fn salvage_symbolic_operation() {
        let content =
            r#"{"op":"Send","step":{"input":{"expression":{"left":3,"operation":"+","right":4}}}}"#;
        let v = try_salvage_json_from_llm_content(content).expect("salvage");
        assert_eq!(v["step"]["input"]["expression"]["operation"], "+");
    }
}
