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
}
