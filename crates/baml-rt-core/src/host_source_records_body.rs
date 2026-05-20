//! Actionable user-visible text for `host.source-records.v1` batches (conversation history).

use serde_json::Value;

/// Formatted poll or unit body for provenance `user` messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressPollBody(pub String);

/// Format lifecycle/source-record rows for a transcript line (no source keys, schema, or record_kind).
#[must_use]
pub fn format_source_records_message_body(batch: &Value) -> IngressPollBody {
    IngressPollBody(format_records_array(
        batch.get("records").and_then(|v| v.as_array()),
    ))
}

/// Format a `withTask` record slice using the same rules as the full poll body.
#[must_use]
pub fn format_source_records_unit_body(records: &[Value]) -> IngressPollBody {
    IngressPollBody(format_records_array(Some(&records.to_vec())))
}

fn format_records_array(records: Option<&Vec<Value>>) -> String {
    let Some(records) = records else {
        return String::new();
    };
    if records.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut index = 0usize;
    for row in records {
        let Some(obj) = row.as_object() else {
            continue;
        };
        if let Some(block) = format_slack_message_row(obj, index) {
            lines.push(block);
            index += 1;
            continue;
        }
        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(title) = title else {
            continue;
        };
        let priority = obj
            .get("priority")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let description = obj
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut line = format!("{}. {title}", index + 1);
        if let Some(priority) = priority {
            line.push_str(&format!(" (priority: {priority})"));
        }
        lines.push(line);
        if let Some(description) = description {
            lines.push(format!("   {description}"));
        }
        index += 1;
    }
    lines.join("\n")
}

fn format_slack_message_row(obj: &serde_json::Map<String, Value>, index: usize) -> Option<String> {
    let record_kind = obj.get("record_kind").and_then(|v| v.as_str())?;
    if record_kind != "slack.message" {
        return None;
    }
    let text = obj
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let channel = obj
        .get("channel_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("channel");
    let speaker = obj
        .get("user_name")
        .or_else(|| obj.get("username"))
        .or_else(|| obj.get("user"))
        .or_else(|| obj.get("user_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("user");
    Some(format!("{}. @{speaker} in #{channel}: {text}", index + 1))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::format_source_records_message_body;

    #[test]
    fn formats_records_without_bookkeeping_fields() {
        let batch = json!({
            "schema_version": "host.source-records.v1",
            "source": { "source_kind": "clickup", "source_key": "k", "source_label": "L" },
            "records": [
                {
                    "record_kind": "clickup.lifecycle_task",
                    "key": "clickup-created:1",
                    "title": "Fix ingress",
                    "description": "Wire poll to history",
                    "priority": "high"
                }
            ]
        });
        let body = format_source_records_message_body(&batch).0;
        assert!(body.contains("1. Fix ingress"));
        assert!(body.contains("(priority: high)"));
        assert!(body.contains("Wire poll to history"));
        assert!(!body.contains("clickup-created"));
        assert!(!body.contains("source_kind"));
    }

    #[test]
    fn formats_slack_message_records() {
        let batch = json!({
            "records": [
                {
                    "record_kind": "slack.message",
                    "channel_id": "C123",
                    "user_name": "alice",
                    "text": "deploy blocked?"
                }
            ]
        });
        let body = format_source_records_message_body(&batch).0;
        assert!(body.contains("@alice in #C123: deploy blocked?"));
    }
}
