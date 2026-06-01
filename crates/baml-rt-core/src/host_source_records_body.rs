// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Wire-faithful text for `host.source-records.v1` ingress rows (conversation history).
//!
//! The host does not interpret records (no title lines, summaries, or field extraction).
//! Agents receive the same JSON in dispatch `messages[0]` and in the unit prelude user row.

use serde_json::{Value, json};

/// Delimiter prefix for ingress user rows (LLM/UI can detect wire JSON bodies).
pub const INGRESS_WIRE_BODY_DELIMITER: &str = "--- host.source-records.v1 ---";

/// Formatted poll or unit body for provenance `user` messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressPollBody(pub String);

/// Format a full batch's `records` array for a poll-level user line (legacy poll path).
#[must_use]
pub fn format_source_records_message_body(batch: &Value) -> IngressPollBody {
    let records = batch
        .get("records")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    format_source_records_wire_body(records)
}

/// Format a `withTask` record slice — canonical ingress prelude for that unit.
#[must_use]
pub fn format_source_records_unit_body(records: &[Value]) -> IngressPollBody {
    format_source_records_wire_body(records)
}

/// Serialize unit `records` as pretty JSON under a fixed delimiter (no semantic rewriting).
#[must_use]
pub fn format_source_records_wire_body(records: &[Value]) -> IngressPollBody {
    let payload = json!({ "records": records });
    let json_text = serde_json::to_string_pretty(&payload)
        .unwrap_or_else(|_| json!({ "records": [] }).to_string());
    IngressPollBody(format!("{INGRESS_WIRE_BODY_DELIMITER}\n{json_text}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        INGRESS_WIRE_BODY_DELIMITER, format_source_records_message_body,
        format_source_records_unit_body,
    };

    #[test]
    fn wire_body_preserves_record_fields() {
        let batch = json!({
            "schema_version": "host.source-records.v1",
            "source": { "source_kind": "clickup", "source_key": "k", "source_label": "L" },
            "records": [
                {
                    "record_kind": "clickup.lifecycle_event",
                    "key": "clickup-created:1",
                    "event": "created",
                    "task_id": "t1",
                    "list_id": "list-1",
                    "snapshot": { "name": "Fix ingress" },
                    "revision": 1
                }
            ]
        });
        let body = format_source_records_message_body(&batch).0;
        assert!(body.starts_with(INGRESS_WIRE_BODY_DELIMITER));
        assert!(body.contains("clickup.lifecycle_event"));
        assert!(body.contains("clickup-created:1"));
        assert!(body.contains("Fix ingress"));
        assert!(!body.contains("1. Fix ingress"));
        assert!(!body.contains("(priority:"));
    }

    #[test]
    fn wire_body_preserves_slack_message_records() {
        let records = vec![json!({
            "record_kind": "slack.message",
            "channel_id": "C123",
            "user_name": "alice",
            "text": "deploy blocked?"
        })];
        let body = format_source_records_unit_body(&records).0;
        assert!(body.contains("slack.message"));
        assert!(body.contains("deploy blocked?"));
        assert!(!body.contains("@alice in #C123"));
    }
}
