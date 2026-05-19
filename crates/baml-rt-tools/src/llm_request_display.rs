//! Normalize LLM request JSON for **human** display (interceptor context, logging).
//!
//! Chat Completions often use `content: [ { "type": "text", "text": "…" } ]` even for plain
//! string-only payloads. Producers and debug views should prefer `content: "…"` for readability
//! and smaller diffs — without changing the wire request (that remains BAML/transport-owned).

use serde_json::{Value, json};

/// If `content` is an array of only `type: "text"` parts (or missing `type` with a `text` field),
/// return a single string (joined with double newlines). Otherwise return `content` unchanged.
/// Non-text or multimodal parts (e.g. `image_url`) are left as the original array.
#[must_use]
pub fn flatten_message_content_value(content: &Value) -> Value {
    let Some(arr) = content.as_array() else {
        return content.clone();
    };
    if arr.is_empty() {
        return json!("");
    }

    let mut texts: Vec<String> = Vec::new();
    for part in arr {
        let Some(obj) = part.as_object() else {
            return content.clone();
        };
        if obj.contains_key("image_url")
            || obj
                .get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|ty| ty != "text")
        {
            return content.clone();
        }
        let Some(s) = obj.get("text").and_then(|t| t.as_str()) else {
            return content.clone();
        };
        texts.push(s.to_string());
    }

    json!(texts.join("\n\n"))
}

/// Recursively find `messages` on the request and flatten each `content` with
/// [`flatten_message_content_value`]. Shallow copy via [`serde_json::to_value`]-style clone: mutates
/// a clone of `request` only.
#[must_use]
pub fn flatten_chat_completion_request_for_display(request: &Value) -> Value {
    let mut v = request.clone();
    if let Some(obj) = v.as_object_mut() {
        if let Some(msgs) = obj.get_mut("messages") {
            flatten_messages_array_in_place(msgs);
        }
        for (_k, sub) in obj.iter_mut() {
            if let Some(msgs) = sub.as_object_mut().and_then(|i| i.get_mut("messages")) {
                flatten_messages_array_in_place(msgs);
            }
        }
    }
    v
}

fn flatten_messages_array_in_place(msgs: &mut Value) {
    let Some(arr) = msgs.as_array_mut() else {
        return;
    };
    for m in arr.iter_mut() {
        let Some(obj) = m.as_object_mut() else {
            continue;
        };
        if let Some(c) = obj.get("content") {
            let new_c = flatten_message_content_value(c);
            if c != &new_c {
                obj.insert("content".to_string(), new_c);
            }
        }
    }
}

/// Unicode scalar count (`str::chars`) for one chat message `content` value.
///
/// String content counts directly. Array content uses the same text-capable rules as
/// [`flatten_message_content_value`]: if every part is a text object, counts the join with `"\n\n"`;
/// otherwise counts only text-capable parts (e.g. text before an image part).
#[must_use]
pub fn message_content_char_count(content: &Value) -> u64 {
    match content {
        Value::String(s) => s.chars().count() as u64,
        Value::Array(arr) => {
            if arr.is_empty() {
                return 0;
            }
            let mut texts: Vec<&str> = Vec::new();
            let mut all_flattenable = true;
            for part in arr {
                let Some(obj) = part.as_object() else {
                    all_flattenable = false;
                    continue;
                };
                if obj.contains_key("image_url")
                    || obj
                        .get("type")
                        .and_then(|t| t.as_str())
                        .is_some_and(|ty| ty != "text")
                {
                    all_flattenable = false;
                    continue;
                }
                let Some(s) = obj.get("text").and_then(|t| t.as_str()) else {
                    all_flattenable = false;
                    continue;
                };
                texts.push(s);
            }
            if all_flattenable && texts.len() == arr.len() {
                return texts.join("\n\n").chars().count() as u64;
            }
            let mut sum = 0u64;
            for part in arr {
                let Some(obj) = part.as_object() else {
                    continue;
                };
                if obj.contains_key("image_url")
                    || obj
                        .get("type")
                        .and_then(|t| t.as_str())
                        .is_some_and(|ty| ty != "text")
                {
                    continue;
                }
                if let Some(s) = obj.get("text").and_then(|t| t.as_str()) {
                    sum += s.chars().count() as u64;
                }
            }
            sum
        }
        _ => 0,
    }
}

fn messages_array_char_count(msgs: &Value) -> u64 {
    let Some(arr) = msgs.as_array() else {
        return 0;
    };
    let mut sum = 0u64;
    for m in arr {
        let Some(obj) = m.as_object() else {
            continue;
        };
        if let Some(c) = obj.get("content") {
            sum += message_content_char_count(c);
        }
    }
    sum
}

/// Sum of [`message_content_char_count`] over every `messages` array on the request body.
///
/// Traversal matches [`flatten_chat_completion_request_for_display`]: top-level `messages` plus
/// one nested `*.messages` on each direct child object. Tool calls and other fields are not counted.
#[must_use]
pub fn prompt_message_char_count(request: &Value) -> u64 {
    let Some(obj) = request.as_object() else {
        return 0;
    };
    let mut total = 0u64;
    if let Some(msgs) = obj.get("messages") {
        total += messages_array_char_count(msgs);
    }
    for (_k, sub) in obj.iter() {
        if let Some(msgs) = sub.as_object().and_then(|inner| inner.get("messages")) {
            total += messages_array_char_count(msgs);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn flattens_single_text_part() {
        let c = json!([{"type": "text", "text": "hello"}]);
        assert_eq!(flatten_message_content_value(&c), json!("hello"));
    }

    #[test]
    fn preserves_string_content() {
        let c = json!("plain");
        assert_eq!(flatten_message_content_value(&c), json!("plain"));
    }

    #[test]
    fn does_not_flatten_image_parts() {
        let c = json!([
            {"type": "text", "text": "hi"},
            {"type": "image_url", "image_url": {"url": "http://x"}}
        ]);
        let out = flatten_message_content_value(&c);
        assert_eq!(out, c);
    }

    #[test]
    fn char_count_string_content() {
        assert_eq!(message_content_char_count(&json!("plain")), 5);
    }

    #[test]
    fn char_count_text_parts_joined() {
        let c = json!([
            {"type": "text", "text": "hello"},
            {"type": "text", "text": "world"},
        ]);
        assert_eq!(
            message_content_char_count(&c),
            "hello\n\nworld".chars().count() as u64
        );
    }

    #[test]
    fn char_count_multimodal_counts_text_only() {
        let c = json!([
            {"type": "text", "text": "hi"},
            {"type": "image_url", "image_url": {"url": "http://x"}},
        ]);
        assert_eq!(message_content_char_count(&c), 2);
    }

    #[test]
    fn prompt_char_count_sums_messages() {
        let req = json!({
            "model": "x",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": [{"type": "text", "text": "a"}]},
            ],
        });
        assert_eq!(prompt_message_char_count(&req), 4);
    }

    #[test]
    fn prompt_char_count_nested_messages() {
        let req = json!({
            "extra": {"messages": [{"role": "user", "content": "nested"}]},
            "messages": [{"role": "user", "content": "top"}],
        });
        assert_eq!(prompt_message_char_count(&req), 9);
    }
}
