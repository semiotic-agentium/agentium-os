//! Shared FalkorDB Cypher result parsing helpers.
//!
//! These functions parse the text-based output format returned by
//! [`text_to_cypher::core::execute_cypher_query`] (with `read_only = true`) into
//! [`serde_json::Value`] structures suitable for further processing.
//!
//! The format uses `Debug`-like wrappers (`String(...)`, `Map({...})`, etc.)
//! and can include unescaped embedded JSON within quoted string values.

use serde_json::Value;

/// Parse the full text output of a FalkorDB read query into a JSON array of row
/// arrays. Each row is a [`Value::Array`] whose elements correspond to the
/// columns in the `RETURN` clause.
pub fn parse_graph_snapshot(raw: &str) -> Option<Value> {
    if raw.trim().is_empty() || raw.trim() == "No results returned." {
        return Some(Value::Array(Vec::new()));
    }
    let mut rows = Vec::new();
    for (line_idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record_str = if let Some(idx) = trimmed.find('[') {
            &trimmed[idx..]
        } else {
            trimmed
        };
        let record_str = record_str.trim();
        if !record_str.starts_with('[') || !record_str.ends_with(']') {
            tracing::debug!(
                line_idx,
                line_preview = %record_str.chars().take(240).collect::<String>(),
                "Skipping FalkorDB line that is not a row record"
            );
            continue;
        }
        let inner = &record_str[1..record_str.len() - 1];
        let parts = split_top_level(inner, ',');
        let mut values = Vec::new();
        let mut row_ok = true;
        for (part_idx, part) in parts.into_iter().enumerate() {
            match parse_debug_value(part.trim()) {
                Some(value) => values.push(value),
                None => {
                    tracing::debug!(
                        line_idx,
                        part_idx,
                        part_preview = %part.chars().take(240).collect::<String>(),
                        "Skipping FalkorDB row due to unparsable value"
                    );
                    row_ok = false;
                    break;
                }
            }
        }
        if !row_ok {
            continue;
        }
        rows.push(Value::Array(values));
    }
    Some(Value::Array(rows))
}

/// Parse the text output into a `Vec<Vec<Value>>` (convenience wrapper over
/// [`parse_graph_snapshot`]).
pub fn parse_rows(raw: &str) -> Vec<Vec<Value>> {
    parse_graph_snapshot(raw)
        .and_then(|parsed| parsed.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.as_array().map(|values| values.to_vec()))
        .collect()
}

/// Split `input` by `delimiter`, respecting nested brackets, braces, parens,
/// and quoted strings (including unescaped embedded JSON inside strings).
pub fn split_top_level(input: &str, delimiter: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth_bracket: usize = 0;
    let mut depth_brace: usize = 0;
    let mut depth_paren: usize = 0;
    let mut in_string = false;
    // FalkorDB returns string properties containing JSON with unescaped
    // inner quotes, e.g. `"{"key":"value"}"`. Track brace/bracket depth
    // inside strings so we only exit the string when the embedded JSON is
    // fully closed.
    let mut string_brace_depth: usize = 0;
    let mut string_bracket_depth: usize = 0;
    let mut escape = false;
    for ch in input.chars() {
        if in_string {
            current.push(ch);
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
            } else if ch == '{' {
                string_brace_depth += 1;
            } else if ch == '}' {
                string_brace_depth = string_brace_depth.saturating_sub(1);
            } else if ch == '[' {
                string_bracket_depth += 1;
            } else if ch == ']' {
                string_bracket_depth = string_bracket_depth.saturating_sub(1);
            } else if ch == '"' && string_brace_depth == 0 && string_bracket_depth == 0 {
                in_string = false;
            }
            // else: stay in string — unescaped quote inside embedded JSON
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                string_brace_depth = 0;
                string_bracket_depth = 0;
                current.push(ch);
            }
            '[' => {
                depth_bracket += 1;
                current.push(ch);
            }
            ']' => {
                depth_bracket = depth_bracket.saturating_sub(1);
                current.push(ch);
            }
            '{' => {
                depth_brace += 1;
                current.push(ch);
            }
            '}' => {
                depth_brace = depth_brace.saturating_sub(1);
                current.push(ch);
            }
            '(' => {
                depth_paren += 1;
                current.push(ch);
            }
            ')' => {
                depth_paren = depth_paren.saturating_sub(1);
                current.push(ch);
            }
            _ if ch == delimiter && depth_bracket == 0 && depth_brace == 0 && depth_paren == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// Parse a single FalkorDB debug-format value into a [`serde_json::Value`].
pub fn parse_debug_value(input: &str) -> Option<Value> {
    let value = input.trim();
    if value.starts_with("Map(") {
        return parse_debug_map(value);
    }
    if value.starts_with("Array(") {
        return parse_debug_array(value);
    }
    if value.starts_with("String(") {
        return parse_debug_string(value).map(Value::String);
    }
    if value.starts_with("I64(") && value.ends_with(')') {
        let inner = &value[4..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("Integer(") && value.ends_with(')') {
        let inner = &value[8..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("Long(") && value.ends_with(')') {
        let inner = &value[5..value.len() - 1];
        return inner
            .parse::<i64>()
            .ok()
            .map(|num| Value::Number(serde_json::Number::from(num)));
    }
    if value.starts_with("F64(") && value.ends_with(')') {
        let inner = &value[4..value.len() - 1];
        return serde_json::Number::from_f64(inner.parse::<f64>().ok()?).map(Value::Number);
    }
    if value.starts_with("Bool(") && value.ends_with(')') {
        let inner = &value[5..value.len() - 1];
        return inner.parse::<bool>().ok().map(Value::Bool);
    }
    if value == "Null" || value == "null" {
        return Some(Value::Null);
    }
    if value.starts_with('[') && value.ends_with(']') {
        return parse_bracket_array(value);
    }
    if value.starts_with('"') && value.ends_with('"') {
        return parse_quoted_string_with_json_fallback(value);
    }
    Some(Value::String(value.to_string()))
}

/// If a [`Value::String`] contains valid JSON, parse it; otherwise return as-is.
pub fn decode_embedded_json(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        other => other.clone(),
    }
}

// ── Private helpers ─────────────────────────────────────────────────────────

fn parse_quoted_string_with_json_fallback(value: &str) -> Option<Value> {
    if let Ok(parsed) = serde_json::from_str::<String>(value) {
        return Some(Value::String(parsed));
    }

    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);

    // FalkorDB text snapshots sometimes return a quoted payload whose inner
    // quotes are unescaped, e.g. `"{"k":"v"}"`. Treat the inner segment as
    // JSON and re-serialize so downstream `decode_embedded_json` can parse it.
    if let Ok(parsed_json) = serde_json::from_str::<Value>(inner) {
        return serde_json::to_string(&parsed_json)
            .ok()
            .map(Value::String)
            .or_else(|| Some(Value::String(inner.to_string())));
    }

    Some(Value::String(inner.to_string()))
}

fn parse_debug_string(value: &str) -> Option<String> {
    if !value.starts_with("String(") || !value.ends_with(')') {
        return None;
    }
    let inner = &value[7..value.len() - 1];
    if inner.starts_with('"') && inner.ends_with('"') {
        if let Ok(parsed) = serde_json::from_str::<String>(inner) {
            Some(parsed)
        } else {
            Some(inner.trim_matches('"').to_string())
        }
    } else {
        let wrapped = format!("\"{}\"", inner);
        if let Ok(parsed) = serde_json::from_str::<String>(&wrapped) {
            Some(parsed)
        } else {
            Some(inner.to_string())
        }
    }
}

fn parse_debug_array(value: &str) -> Option<Value> {
    if !value.starts_with("Array(") || !value.ends_with(')') {
        return None;
    }
    let inner = &value[6..value.len() - 1];
    parse_bracket_array(inner)
}

fn parse_bracket_array(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if inner.trim().is_empty() {
        return Some(Value::Array(Vec::new()));
    }
    let parts = split_top_level(inner, ',');
    let mut values = Vec::new();
    for part in parts {
        values.push(parse_debug_value(part.trim())?);
    }
    Some(Value::Array(values))
}

fn parse_debug_map(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    if !trimmed.starts_with("Map(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed[4..trimmed.len() - 1];
    let inner = inner.trim();
    let inner = if inner.starts_with('{') && inner.ends_with('}') {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };
    if inner.trim().is_empty() {
        return Some(Value::Object(serde_json::Map::new()));
    }
    let parts = split_top_level(inner, ',');
    let mut map = serde_json::Map::new();
    for part in parts {
        let mut iter = split_top_level(part.trim(), ':').into_iter();
        let key_raw = iter.next()?.trim().to_string();
        let value_raw = iter.collect::<Vec<String>>().join(":");
        let key = serde_json::from_str::<String>(&key_raw).ok()?;
        let value = parse_debug_value(value_raw.trim())?;
        map.insert(key, value);
    }
    Some(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_debug_value_handles_unescaped_json_inside_quoted_string() {
        let raw = r#""{"correlation_id":"corr-123","message_id":"cli-msg-1"}""#;
        let parsed = parse_debug_value(raw).expect("quoted value should parse");
        assert_eq!(
            parsed,
            Value::String(r#"{"correlation_id":"corr-123","message_id":"cli-msg-1"}"#.to_string())
        );

        let decoded = decode_embedded_json(&parsed);
        assert_eq!(decoded["correlation_id"], "corr-123");
        assert_eq!(decoded["message_id"], "cli-msg-1");
    }

    #[test]
    fn split_top_level_does_not_split_unescaped_json_string_body() {
        let input = r#""{"correlation_id":"corr-123","message_id":"cli-msg-1"}", "assistant""#;
        let parts = split_top_level(input, ',');
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            r#""{"correlation_id":"corr-123","message_id":"cli-msg-1"}""#
        );
        assert_eq!(parts[1], r#""assistant""#);
    }

    #[test]
    fn parse_graph_snapshot_keeps_tool_rows_with_unescaped_json_columns() {
        let raw = r#"["prov-11", "support/clickup", "{"correlation_id":"corr-123","result":{"items":[{"id":"901"}]}}", "{"action":"ListTeams"}", "a2a:args", "a2a:ToolArgs"]"#;
        let parsed = parse_graph_snapshot(raw).expect("snapshot should parse");
        let rows = parsed.as_array().expect("rows array");
        assert_eq!(rows.len(), 1);
        let cols = rows[0].as_array().expect("row columns");
        assert_eq!(cols.len(), 6);

        let metadata = decode_embedded_json(&cols[2]);
        let args = decode_embedded_json(&cols[3]);
        assert_eq!(metadata["correlation_id"], "corr-123");
        assert_eq!(metadata["result"]["items"][0]["id"], "901");
        assert_eq!(args["action"], "ListTeams");
    }

    #[test]
    fn parse_rows_returns_empty_for_no_results() {
        assert!(parse_rows("").is_empty());
        assert!(parse_rows("No results returned.").is_empty());
    }

    #[test]
    fn parse_rows_returns_columns() {
        let raw = r#"["prov-1", "hello"]"#;
        let rows = parse_rows(raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0], Value::String("prov-1".to_string()));
        assert_eq!(rows[0][1], Value::String("hello".to_string()));
    }
}
