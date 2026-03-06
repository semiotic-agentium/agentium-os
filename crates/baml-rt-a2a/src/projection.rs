//! Conversation context projection pipeline.
//!
//! This module implements the read-time, non-destructive projection of immutable
//! provenance events into prompt-ready context.
//!
//! The primary entry point is [`project`], a pure function that takes candidate
//! provenance items and a [`ProjectionConfig`], applies compaction and budgeting,
//! and returns the serialized envelope plus projection statistics.

use std::collections::HashSet;

use baml_rt_provenance::ProvenanceConversationContextItem;
use baml_rt_tools::ToolRegistry;
use serde_json::Value;

/// Allowed source types for the projection pipeline.
const DEFAULT_ALLOWED_SOURCES: &[&str] = &["message", "tool_result"];

/// Configuration for a single projection invocation.
#[derive(Debug, Clone)]
pub struct ProjectionConfig {
    /// Hard cap on the number of projected items.
    pub max_items: usize,
    /// Source types to include. Items whose `source` field is not in this set
    /// are dropped before any other pipeline stage. Default: `["message", "tool_result"]`.
    pub allowed_sources: HashSet<String>,
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        Self {
            max_items: 40,
            allowed_sources: DEFAULT_ALLOWED_SOURCES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

/// Statistics collected during a projection pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectionStats {
    /// Total candidate items received from the store.
    pub candidates: usize,
    /// Total items in the projected output.
    pub projected: usize,
    /// Total estimated chars in the serialized projected output.
    pub projected_chars: usize,
    /// Items dropped because their `source` was not in `allowed_sources`.
    pub dropped_source_filtered: usize,
    /// Items dropped by budget truncation.
    pub dropped_budgeted: usize,
}

/// Project provenance conversation context items into the prompt event envelope.
///
/// This is a **pure function**: it does not perform I/O, does not touch the store,
/// and is fully deterministic for a fixed input. The caller is responsible for
/// reading candidates from the store and passing the tool registry for compaction.
///
/// # Pipeline stages (current — Task 1 baseline)
///
/// 1. Tool compaction (`compact_result` per tool_result).
/// 2. Budget truncation (`max_items`, keeping the latest N items).
/// 3. Serialize to prompt event envelope (`{role, source, content}`).
///
/// Subsequent tasks will insert additional stages (source filtering, dedupe,
/// empty-content removal, floor guarantee, projection modes) between these steps.
pub fn project(
    mut items: Vec<ProvenanceConversationContextItem>,
    config: &ProjectionConfig,
    tool_registry: &ToolRegistry,
) -> (Vec<Value>, ProjectionStats) {
    let candidates = items.len();

    // Stage: source filtering — drop items whose source type is not in the
    // allowed set. This runs before compaction so we don't waste work on
    // items that will be discarded anyway.
    let pre_filter_len = items.len();
    items.retain(|item| config.allowed_sources.contains(&item.source));
    let dropped_source_filtered = pre_filter_len - items.len();

    // Stage: tool compaction (read-time, in-memory only).
    for item in &mut items {
        if item.source != "tool_result" {
            continue;
        }
        let Some(tool_name) = item.content.get("tool_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(handler) = tool_registry.get_handler(tool_name) else {
            continue;
        };
        handler.compact_result(&mut item.content);
    }

    // Stage: budget truncation (keep latest N by position; items are already
    // in chronological order from the store).
    let dropped_budgeted = if items.len() > config.max_items {
        let overflow = items.len() - config.max_items;
        items = items.split_off(overflow);
        overflow
    } else {
        0
    };

    // Stage: serialize to prompt event envelope.
    let mut projected_chars: usize = 0;
    let entries: Vec<Value> = items
        .into_iter()
        .map(|item| {
            let content = match &item.content {
                Value::String(s) => s.clone(),
                other => content_to_string(other),
            };
            projected_chars += content.len();
            serde_json::json!({
                "role": item.role,
                "source": item.source,
                "content": content,
            })
        })
        .collect();

    let stats = ProjectionStats {
        candidates,
        projected: entries.len(),
        projected_chars,
        dropped_source_filtered,
        dropped_budgeted,
    };

    (entries, stats)
}

/// Serialize a non-string `Value` to a JSON string for prompt injection.
fn content_to_string(v: &Value) -> String {
    serde_json::to_string(v)
        .inspect_err(|e| {
            tracing::warn!(
                error = %e,
                "conversation context content serialization failed, using Debug"
            );
        })
        .unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::EventId;
    use baml_rt_tools::ToolRegistry;
    use serde_json::json;

    use super::*;

    /// Build a synthetic provenance item for testing.
    fn make_item(
        role: &str,
        source: &str,
        content: Value,
        timestamp_ms: u64,
    ) -> ProvenanceConversationContextItem {
        ProvenanceConversationContextItem {
            timestamp_ms,
            event_id: EventId::from(format!("evt-{}", timestamp_ms).as_str()),
            role: role.to_string(),
            content,
            source: source.to_string(),
        }
    }

    fn empty_registry() -> ToolRegistry {
        ToolRegistry::new()
    }

    #[test]
    fn test_empty_input_returns_empty() {
        let config = ProjectionConfig::default();
        let (entries, stats) = project(vec![], &config, &empty_registry());
        assert!(entries.is_empty());
        assert_eq!(stats.candidates, 0);
        assert_eq!(stats.projected, 0);
    }

    #[test]
    fn test_basic_projection_preserves_order_and_envelope() {
        let items = vec![
            make_item("ROLE_USER", "message", Value::String("hello".into()), 1000),
            make_item(
                "tool",
                "tool_result",
                json!({"tool_name": "clickup/create_task", "result": {"id": "123"}}),
                2000,
            ),
            make_item("ROLE_AGENT", "message", Value::String("done".into()), 3000),
        ];

        let config = ProjectionConfig::default();
        let (entries, stats) = project(items, &config, &empty_registry());

        assert_eq!(stats.candidates, 3);
        assert_eq!(stats.projected, 3);
        assert_eq!(stats.dropped_source_filtered, 0);

        // Verify envelope shape and chronological ordering.
        assert_eq!(entries[0]["role"], "ROLE_USER");
        assert_eq!(entries[0]["content"], "hello");
        assert_eq!(entries[1]["source"], "tool_result");
        assert_eq!(entries[2]["role"], "ROLE_AGENT");
        assert_eq!(entries[2]["content"], "done");
    }

    #[test]
    fn test_string_content_not_double_serialized() {
        let items = vec![make_item(
            "ROLE_USER",
            "message",
            Value::String("plain text".into()),
            1000,
        )];

        let config = ProjectionConfig::default();
        let (entries, _) = project(items, &config, &empty_registry());

        // String content should be passed through as-is, not JSON-escaped.
        assert_eq!(entries[0]["content"], "plain text");
    }

    #[test]
    fn test_budget_truncation_keeps_latest() {
        let items: Vec<_> = (0..10)
            .map(|i| {
                make_item(
                    "ROLE_USER",
                    "message",
                    Value::String(format!("msg-{}", i)),
                    i * 1000,
                )
            })
            .collect();

        let config = ProjectionConfig {
            max_items: 3,
            ..ProjectionConfig::default()
        };
        let (entries, stats) = project(items, &config, &empty_registry());

        assert_eq!(stats.candidates, 10);
        assert_eq!(stats.projected, 3);
        assert_eq!(stats.dropped_budgeted, 7);

        // Should keep the last 3 (msg-7, msg-8, msg-9).
        let contents: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.get("content").and_then(Value::as_str))
            .collect();
        assert_eq!(contents, vec!["msg-7", "msg-8", "msg-9"]);
    }

    #[test]
    fn test_projected_chars_counted() {
        let items = vec![
            make_item("ROLE_USER", "message", Value::String("abcde".into()), 1000),
            make_item("ROLE_USER", "message", Value::String("fg".into()), 2000),
        ];

        let config = ProjectionConfig::default();
        let (_, stats) = project(items, &config, &empty_registry());

        // "abcde" (5) + "fg" (2) = 7
        assert_eq!(stats.projected_chars, 7);
    }

    #[test]
    fn test_object_content_serialized_to_json_string() {
        let items = vec![make_item(
            "tool",
            "tool_result",
            json!({"key": "value"}),
            1000,
        )];

        let config = ProjectionConfig::default();
        let (entries, _) = project(items, &config, &empty_registry());

        // Object content should be serialized to a JSON string.
        let content = entries[0]["content"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).expect("should be valid JSON");
        assert_eq!(parsed, json!({"key": "value"}));
    }

    #[test]
    fn test_source_filtering_drops_disallowed_keeps_allowed() {
        let items = vec![
            make_item("ROLE_USER", "message", Value::String("hi".into()), 1000),
            make_item("assistant", "tool_call", json!({"tool_call": {}}), 2000),
            make_item("tool", "tool_result", json!({"result": {}}), 3000),
            make_item("system", "internal", Value::String("noise".into()), 4000),
        ];

        // Default: ["message", "tool_result"] — tool_call and internal dropped.
        let config = ProjectionConfig::default();
        let (entries, stats) = project(items, &config, &empty_registry());

        assert_eq!(stats.candidates, 4);
        assert_eq!(stats.dropped_source_filtered, 2);
        assert_eq!(stats.projected, 2);
        let sources: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["source"].as_str())
            .collect();
        assert_eq!(sources, vec!["message", "tool_result"]);
    }
}
