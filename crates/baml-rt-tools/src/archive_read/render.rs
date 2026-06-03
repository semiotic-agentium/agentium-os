// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON to YAML rendering for grep-friendly line-based access.
//!
//! Long strings become YAML block scalars (`>-`) so they span multiple
//! individually-addressable lines instead of one 10KB mega-line.
//! No external YAML library — hand-rendered subset.

use serde_json::Value;

use super::rendered::RenderedContent;

const BLOCK_SCALAR_THRESHOLD: usize = 80;
const WRAP_WIDTH: usize = 78;

/// Render a JSON value to grep-friendly YAML lines.
pub fn render_to_lines(value: &Value) -> RenderedContent {
    let mut lines = Vec::new();
    match value {
        Value::Array(arr) if arr.is_empty() => {
            lines.push("(empty)".to_string());
        }
        Value::Array(arr) if arr.iter().all(Value::is_object) => {
            render_object_array(arr, &mut lines);
        }
        Value::Array(arr) => {
            for item in arr {
                lines.push(render_scalar(item));
            }
        }
        Value::Object(_) => {
            render_mapping(value, 0, &mut lines);
        }
        Value::Null => lines.push("null".to_string()),
        _ => lines.push(render_scalar(value)),
    }
    RenderedContent::from_lines(lines)
}

fn render_object_array(arr: &[Value], lines: &mut Vec<String>) {
    for obj in arr {
        let fields = obj.as_object().expect("checked in caller");
        render_object_item(fields, "", 2, lines);
    }
}

/// Render one object as a YAML sequence item using `base_indent` for the
/// `"- "` / `"  "` first-vs-continuation prefix.
fn render_object_item(
    fields: &serde_json::Map<String, Value>,
    base_indent: &str,
    continuation_indent: usize,
    lines: &mut Vec<String>,
) {
    for (fi, (key, val)) in fields.iter().enumerate() {
        let prefix = if fi == 0 {
            format!("{base_indent}- ")
        } else {
            format!("{base_indent}  ")
        };
        render_field(&prefix, key, val, continuation_indent, lines);
    }
}

fn render_mapping(value: &Value, indent: usize, lines: &mut Vec<String>) {
    let Some(obj) = value.as_object() else {
        lines.push(format!("{}{}", " ".repeat(indent), render_scalar(value)));
        return;
    };
    for (key, val) in obj {
        render_field(&" ".repeat(indent), key, val, indent + 2, lines);
    }
}

fn render_field(
    prefix: &str,
    key: &str,
    val: &Value,
    continuation_indent: usize,
    lines: &mut Vec<String>,
) {
    match val {
        Value::String(s) if needs_block_scalar(s) => {
            lines.push(format!("{prefix}{key}: >-"));
            let indent = " ".repeat(continuation_indent);
            for wrapped in wrap_text(s, WRAP_WIDTH.saturating_sub(continuation_indent)) {
                lines.push(format!("{indent}{wrapped}"));
            }
        }
        Value::String(s) => {
            lines.push(format!("{prefix}{key}: {}", quote_if_needed(s)));
        }
        Value::Object(_) => {
            lines.push(format!("{prefix}{key}:"));
            render_mapping(val, continuation_indent, lines);
        }
        Value::Array(arr) if arr.is_empty() => {
            lines.push(format!("{prefix}{key}: []"));
        }
        Value::Array(arr) if arr.iter().all(|v| !v.is_object() && !v.is_array()) => {
            let items: Vec<String> = arr.iter().map(render_scalar).collect();
            let inline = format!("{prefix}{key}: [{}]", items.join(", "));
            if inline.len() <= BLOCK_SCALAR_THRESHOLD {
                lines.push(inline);
            } else {
                lines.push(format!("{prefix}{key}:"));
                let indent = " ".repeat(continuation_indent);
                for item in arr {
                    lines.push(format!("{indent}- {}", render_scalar(item)));
                }
            }
        }
        Value::Array(arr) => {
            lines.push(format!("{prefix}{key}:"));
            let indent = " ".repeat(continuation_indent);
            for item in arr {
                if let Some(obj) = item.as_object() {
                    render_object_item(obj, &indent, continuation_indent + 4, lines);
                } else {
                    lines.push(format!("{indent}- {}", render_scalar(item)));
                }
            }
        }
        _ => {
            lines.push(format!("{prefix}{key}: {}", render_scalar(val)));
        }
    }
}

fn needs_block_scalar(s: &str) -> bool {
    s.len() > BLOCK_SCALAR_THRESHOLD || s.contains('\n')
}

fn render_scalar(val: &Value) -> String {
    match val {
        Value::String(s) => quote_if_needed(s),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        _ => val.to_string(),
    }
}

/// Quote a string value if it contains YAML-ambiguous characters.
fn quote_if_needed(s: &str) -> String {
    if s.is_empty()
        || s.contains(": ")
        || s.contains(" #")
        || s.starts_with('#')
        || s.starts_with('\'')
        || s.starts_with('"')
        || s.starts_with('{')
        || s.starts_with('[')
        || s.starts_with('*')
        || s.starts_with('&')
        || s.starts_with('!')
        || s.starts_with('|')
        || s.starts_with('>')
        || s.starts_with('%')
        || s.starts_with('@')
        || s.starts_with('`')
        || s.contains('\n')
        || is_yaml_keyword(s)
    {
        format!("\"{}\"", escape_yaml_string(s))
    } else {
        s.to_string()
    }
}

fn is_yaml_keyword(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        "null" | "true" | "false" | "yes" | "no" | "on" | "off" | "~"
    )
}

fn escape_yaml_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// Wrap text at word boundaries to fit within `max_width`.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(20);
    let mut result = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            result.push(String::new());
            continue;
        }
        let words: Vec<&str> = paragraph.split_whitespace().collect();
        if words.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        for word in &words {
            if current_line.is_empty() {
                current_line = word.to_string();
            } else if current_line.len() + 1 + word.len() <= max_width {
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                result.push(current_line);
                current_line = word.to_string();
            }
        }
        if !current_line.is_empty() {
            result.push(current_line);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn render_to_lines_matrix() {
        struct Case {
            label: &'static str,
            value: serde_json::Value,
            check: fn(&RenderedContent),
        }

        let cases: &[Case] = &[
            Case {
                label: "scalar_string",
                value: json!("hello world"),
                check: |rc| {
                    assert_eq!(rc.line_count(), 1);
                    assert_eq!(rc.get_line(0), Some("hello world"));
                },
            },
            Case {
                label: "scalar_number",
                value: json!(42),
                check: |rc| assert_eq!(rc.get_line(0), Some("42")),
            },
            Case {
                label: "null_value",
                value: json!(null),
                check: |rc| assert_eq!(rc.get_line(0), Some("null")),
            },
            Case {
                label: "empty_array",
                value: json!([]),
                check: |rc| assert_eq!(rc.get_line(0), Some("(empty)")),
            },
            Case {
                label: "array_of_scalars",
                value: json!(["a", "b", "c"]),
                check: |rc| {
                    assert_eq!(rc.lines().collect::<Vec<_>>(), vec!["a", "b", "c"]);
                },
            },
            Case {
                label: "simple_object",
                value: json!({"name": "alice", "age": 30}),
                check: |rc| {
                    let lines: Vec<&str> = rc.lines().collect();
                    assert!(lines.contains(&"name: alice"));
                    assert!(lines.contains(&"age: 30"));
                },
            },
            Case {
                label: "array_of_objects",
                value: json!([
                    {"user": "alice", "role": "admin"},
                    {"user": "bob", "role": "viewer"}
                ]),
                check: |rc| {
                    let lines: Vec<&str> = rc.lines().collect();
                    assert!(lines.contains(&"- user: alice"));
                    assert!(lines.contains(&"  role: admin"));
                    assert!(lines.contains(&"- user: bob"));
                },
            },
            Case {
                label: "yaml_keyword_quoted",
                value: json!({"val": "null"}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"null\""))),
            },
            Case {
                label: "colon_space_quoted",
                value: json!({"channel": "#general: main"}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"#general: main\""))),
            },
            Case {
                label: "hash_prefix_quoted",
                value: json!({"channel": "#general"}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"#general\""))),
            },
            Case {
                label: "empty_string_quoted",
                value: json!({"val": ""}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"\""))),
            },
        ];

        for case in cases {
            let rc = render_to_lines(&case.value);
            (case.check)(&rc);
            assert!(!case.label.is_empty(), "matrix case label");
        }

        let long_text = "a ".repeat(100);
        let rc = render_to_lines(&json!({"message": long_text.trim()}));
        let lines: Vec<&str> = rc.lines().collect();
        assert_eq!(lines[0], "message: >-", "long_string block opener");
        assert!(lines.len() > 2);
        for line in &lines[1..] {
            assert!(line.len() <= WRAP_WIDTH + 5, "wrapped line too long");
        }

        let rc = render_to_lines(&json!({"note": "line one\nline two\nline three"}));
        let lines: Vec<&str> = rc.lines().collect();
        assert_eq!(lines[0], "note: >-");
        assert!(lines.iter().any(|l| l.trim() == "line one"));
        assert!(lines.iter().any(|l| l.trim() == "line two"));

        let huge = "deployment ".repeat(1000);
        let rc = render_to_lines(&json!({"body": huge.trim()}));
        assert!(rc.line_count() > 50, "10KB body line count");
        assert!(rc.lines().any(|l| l.contains("deployment")));
    }
}
