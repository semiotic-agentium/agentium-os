//! Shared string-processing utilities.

/// Truncate `input` to at most `max_chars` **characters** (not bytes).
///
/// If truncation occurs the result is capped at `max_chars` characters
/// followed by `"..."`.  Inputs that fit within the limit are returned
/// unchanged.
pub(crate) fn truncate_chars_with_ellipsis(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars) + 3);
    for (i, ch) in input.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

/// Trim whitespace then truncate an owned [`String`] in place.
///
/// Empty or whitespace-only strings are left as empty.  Otherwise the
/// string is trimmed and, if it exceeds `max_chars` characters, truncated
/// with an ellipsis via [`truncate_chars_with_ellipsis`].
// Reserved for struct-level compaction; production code currently uses the
// JSON-level helpers, but these remain for direct `String` manipulation.
#[allow(dead_code)]
pub(crate) fn trim_and_truncate(text: &mut String, max_chars: usize) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if trimmed.chars().count() > max_chars {
        *text = truncate_chars_with_ellipsis(trimmed, max_chars);
    } else if trimmed.len() != text.len() {
        *text = trimmed.to_string();
    }
}

/// Like [`trim_and_truncate`] but for `Option<String>`.
///
/// `None` and whitespace-only values become `None`.
// Reserved for struct-level compaction; see `trim_and_truncate` above.
#[allow(dead_code)]
pub(crate) fn trim_and_truncate_option(text: &mut Option<String>, max_chars: usize) {
    let Some(current) = text.as_ref() else {
        return;
    };
    let trimmed = current.trim();
    if trimmed.is_empty() {
        *text = None;
        return;
    }
    if trimmed.chars().count() > max_chars {
        *text = Some(truncate_chars_with_ellipsis(trimmed, max_chars));
    } else if trimmed.len() != current.len() {
        *text = Some(trimmed.to_string());
    }
}

// ---------------------------------------------------------------------------
// JSON-level in-place string compaction
// ---------------------------------------------------------------------------

/// Trim and truncate a required string field inside a [`serde_json::Value`]
/// object.
///
/// If the field is missing or not a string this is a no-op.
pub fn trim_and_truncate_json_field(obj: &mut serde_json::Value, field: &str, max_chars: usize) {
    let Some(val) = obj.get_mut(field) else {
        return;
    };
    let new = match val.as_str() {
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return;
            }
            if trimmed.chars().count() > max_chars {
                Some(serde_json::Value::String(truncate_chars_with_ellipsis(
                    trimmed, max_chars,
                )))
            } else if trimmed.len() != s.len() {
                Some(serde_json::Value::String(trimmed.to_string()))
            } else {
                None
            }
        }
        None => None,
    };
    if let Some(replacement) = new {
        *val = replacement;
    }
}

/// Trim and truncate an optional string field inside a [`serde_json::Value`]
/// object, removing the key entirely when the value is null or whitespace-only.
pub fn trim_and_truncate_json_field_option(
    obj: &mut serde_json::Value,
    field: &str,
    max_chars: usize,
) {
    let Some(container) = obj.as_object_mut() else {
        return;
    };

    let action = match container.get(field) {
        None => return,
        Some(v) if v.is_null() => FieldAction::Remove,
        Some(v) => match v.as_str() {
            None => return,
            Some(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    FieldAction::Remove
                } else if trimmed.chars().count() > max_chars {
                    FieldAction::Replace(serde_json::Value::String(truncate_chars_with_ellipsis(
                        trimmed, max_chars,
                    )))
                } else if trimmed.len() != s.len() {
                    FieldAction::Replace(serde_json::Value::String(trimmed.to_string()))
                } else {
                    FieldAction::Keep
                }
            }
        },
    };

    match action {
        FieldAction::Keep => {}
        FieldAction::Remove => {
            container.remove(field);
        }
        FieldAction::Replace(v) => {
            container.insert(field.to_string(), v);
        }
    }
}

/// Remove a string-valued key from a JSON object, returning its value.
///
/// Only removes the key when the value is a JSON string.  Non-string
/// values (numbers, booleans, objects, …) are left untouched and the
/// function returns `None`.
pub fn remove_json_string_field(obj: &mut serde_json::Value, field: &str) -> Option<String> {
    let map = obj.as_object_mut()?;
    if !map.get(field).is_some_and(serde_json::Value::is_string) {
        return None;
    }
    match map.remove(field) {
        Some(serde_json::Value::String(s)) => Some(s),
        _ => None, // unreachable given the guard above
    }
}

enum FieldAction {
    Keep,
    Remove,
    Replace(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_below_limit_unchanged() {
        assert_eq!(truncate_chars_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_limit_unchanged() {
        assert_eq!(truncate_chars_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_above_limit_adds_ellipsis() {
        assert_eq!(truncate_chars_with_ellipsis("hello world", 5), "hello...");
    }

    #[test]
    fn truncate_multi_byte_chars() {
        // 3 chars, limit 2 → keeps 2 chars + "..."
        assert_eq!(truncate_chars_with_ellipsis("héllo", 2), "hé...");
    }

    // -----------------------------------------------------------------------
    // trim_and_truncate
    // -----------------------------------------------------------------------

    #[test]
    fn trim_and_truncate_empty_is_noop() {
        let mut val = String::new();
        trim_and_truncate(&mut val, 10);
        assert_eq!(val, "");
    }

    #[test]
    fn trim_and_truncate_within_limit_unchanged() {
        let mut val = "short".to_string();
        trim_and_truncate(&mut val, 10);
        assert_eq!(val, "short");
    }

    #[test]
    fn trim_and_truncate_at_boundary_unchanged() {
        let mut val = "12345".to_string();
        trim_and_truncate(&mut val, 5);
        assert_eq!(val, "12345");
    }

    #[test]
    fn trim_and_truncate_over_boundary_truncates() {
        let mut val = "x".repeat(400);
        trim_and_truncate(&mut val, 10);
        assert!(val.ends_with("..."));
        assert_eq!(val.chars().count(), 13);
    }

    #[test]
    fn trim_and_truncate_multi_byte() {
        let mut val = "ñ".repeat(20);
        trim_and_truncate(&mut val, 5);
        assert!(val.ends_with("..."));
        assert_eq!(val.chars().count(), 8); // 5 + "..."
    }

    // -----------------------------------------------------------------------
    // trim_and_truncate_option
    // -----------------------------------------------------------------------

    #[test]
    fn trim_and_truncate_option_none_is_noop() {
        let mut val: Option<String> = None;
        trim_and_truncate_option(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn trim_and_truncate_option_empty_becomes_none() {
        let mut val = Some(String::new());
        trim_and_truncate_option(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn trim_and_truncate_option_whitespace_becomes_none() {
        let mut val = Some("   ".to_string());
        trim_and_truncate_option(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn trim_and_truncate_option_within_limit_unchanged() {
        let mut val = Some("short".to_string());
        trim_and_truncate_option(&mut val, 10);
        assert_eq!(val.as_deref(), Some("short"));
    }

    #[test]
    fn trim_and_truncate_option_over_limit_truncates() {
        let mut val = Some("a]".repeat(150));
        trim_and_truncate_option(&mut val, 10);
        let result = val.unwrap();
        assert!(result.ends_with("..."));
        // 10 chars + "..."
        assert_eq!(result.chars().count(), 13);
    }

    // -----------------------------------------------------------------------
    // trim_and_truncate_json_field (required string)
    // -----------------------------------------------------------------------

    #[test]
    fn json_field_missing_is_noop() {
        let mut obj = serde_json::json!({"other": "value"});
        trim_and_truncate_json_field(&mut obj, "text", 10);
        assert_eq!(obj, serde_json::json!({"other": "value"}));
    }

    #[test]
    fn json_field_non_string_is_noop() {
        let mut obj = serde_json::json!({"text": 42});
        trim_and_truncate_json_field(&mut obj, "text", 10);
        assert_eq!(obj, serde_json::json!({"text": 42}));
    }

    #[test]
    fn json_field_within_limit_unchanged() {
        let mut obj = serde_json::json!({"text": "short"});
        trim_and_truncate_json_field(&mut obj, "text", 10);
        assert_eq!(obj["text"], "short");
    }

    #[test]
    fn json_field_over_limit_truncates() {
        let long = "x".repeat(400);
        let mut obj = serde_json::json!({"text": long});
        trim_and_truncate_json_field(&mut obj, "text", 10);
        let result = obj["text"].as_str().unwrap();
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 13);
    }

    #[test]
    fn json_field_trims_whitespace() {
        let mut obj = serde_json::json!({"text": "  hello  "});
        trim_and_truncate_json_field(&mut obj, "text", 10);
        assert_eq!(obj["text"], "hello");
    }

    // -----------------------------------------------------------------------
    // trim_and_truncate_json_field_option (nullable string — removes key)
    // -----------------------------------------------------------------------

    #[test]
    fn json_option_missing_is_noop() {
        let mut obj = serde_json::json!({"other": "value"});
        trim_and_truncate_json_field_option(&mut obj, "text", 10);
        assert_eq!(obj, serde_json::json!({"other": "value"}));
    }

    #[test]
    fn json_option_null_removes_key() {
        let mut obj = serde_json::json!({"text": null});
        trim_and_truncate_json_field_option(&mut obj, "text", 10);
        assert!(obj.get("text").is_none());
    }

    #[test]
    fn json_option_empty_removes_key() {
        let mut obj = serde_json::json!({"text": ""});
        trim_and_truncate_json_field_option(&mut obj, "text", 10);
        assert!(obj.get("text").is_none());
    }

    #[test]
    fn json_option_whitespace_removes_key() {
        let mut obj = serde_json::json!({"text": "   "});
        trim_and_truncate_json_field_option(&mut obj, "text", 10);
        assert!(obj.get("text").is_none());
    }

    #[test]
    fn json_option_within_limit_unchanged() {
        let mut obj = serde_json::json!({"text": "short"});
        trim_and_truncate_json_field_option(&mut obj, "text", 10);
        assert_eq!(obj["text"], "short");
    }

    #[test]
    fn json_option_over_limit_truncates() {
        let long = "x".repeat(400);
        let mut obj = serde_json::json!({"text": long});
        trim_and_truncate_json_field_option(&mut obj, "text", 10);
        let result = obj["text"].as_str().unwrap();
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 13);
    }

    // -----------------------------------------------------------------------
    // remove_json_string_field
    // -----------------------------------------------------------------------

    #[test]
    fn remove_json_string_present() {
        let mut obj = serde_json::json!({"op": "get_page_blocks", "x": 1});
        let val = remove_json_string_field(&mut obj, "op");
        assert_eq!(val.as_deref(), Some("get_page_blocks"));
        assert!(obj.get("op").is_none());
    }

    #[test]
    fn remove_json_string_missing() {
        let mut obj = serde_json::json!({"x": 1});
        let val = remove_json_string_field(&mut obj, "op");
        assert!(val.is_none());
    }

    #[test]
    fn remove_json_string_non_string_preserved() {
        let mut obj = serde_json::json!({"op": 42});
        let val = remove_json_string_field(&mut obj, "op");
        assert!(val.is_none());
        // Non-string value must be left in place
        assert_eq!(obj["op"], 42);
    }
}
