//! Task-local stream-yield sender so tool-session streaming outputs can be pushed
//! into the same channel as __baml_stream results (see docs/argument-sketch-stream-trace.md).

use serde_json::{Map, Value};
use tokio::sync::mpsc;

tokio::task_local! {
    /// Set by __baml_stream before stream.run(); read by execute_tool_session_plan when
    /// returning Value::Array(streaming_outputs) so those chunks are emitted into the stream.
    pub(crate) static STREAM_YIELD_SENDER: Option<mpsc::Sender<Value>>;
}

/// Run `f` with the optional stream-yield sender set. Used by __baml_stream before stream.run().
pub(crate) async fn scope_stream_yield<R, F>(sender: Option<mpsc::Sender<Value>>, f: F) -> R
where
    F: std::future::Future<Output = R>,
{
    STREAM_YIELD_SENDER.scope(sender, f).await
}

/// Key added to tool stream chunks so clients can show which tool produced the events.
pub const TOOL_CHUNK_TOOL_NAME_KEY: &str = "toolName";

/// Decorates a tool stream chunk with the tool name if not already present.
/// - If `value` is an object and has no `toolName` key: insert `toolName`.
/// - If `value` is an object and already has `toolName`: return as-is.
/// - If `value` is not an object: wrap as `{ toolName, chunk: value }`.
pub(crate) fn decorate_tool_chunk(tool_name: &str, value: &Value) -> Value {
    if let Some(obj) = value.as_object() {
        if obj.contains_key(TOOL_CHUNK_TOOL_NAME_KEY) {
            return value.clone();
        }
        let mut out = obj.clone();
        out.insert(
            TOOL_CHUNK_TOOL_NAME_KEY.to_string(),
            Value::String(tool_name.to_string()),
        );
        return Value::Object(out);
    }
    let mut wrap = Map::new();
    wrap.insert(
        TOOL_CHUNK_TOOL_NAME_KEY.to_string(),
        Value::String(tool_name.to_string()),
    );
    wrap.insert("chunk".to_string(), value.clone());
    Value::Object(wrap)
}

/// If a stream-yield sender is set, send one chunk immediately (incremental streaming).
/// Called from execute_tool_session_plan each time we get ToolStep::Streaming or Suspended.
/// Chunk should already be decorated with tool name (use decorate_tool_chunk before calling).
pub(crate) fn send_tool_stream_chunk(value: &Value) {
    let _ = STREAM_YIELD_SENDER.try_with(|opt| {
        if let Some(sender) = opt {
            let _ = sender.try_send(value.clone());
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn decorate_tool_chunk_inserts_tool_name_when_object_has_none() {
        let value = json!({ "events": [], "completion": null });
        let out = decorate_tool_chunk("test/streaming_tool", &value);
        assert_eq!(
            out.get(TOOL_CHUNK_TOOL_NAME_KEY).and_then(Value::as_str),
            Some("test/streaming_tool")
        );
        assert_eq!(out.get("events"), value.get("events"));
    }

    #[test]
    fn decorate_tool_chunk_preserves_existing_tool_name() {
        let value = json!({ "toolName": "already/set", "events": [] });
        let out = decorate_tool_chunk("other/tool", &value);
        assert_eq!(
            out.get(TOOL_CHUNK_TOOL_NAME_KEY).and_then(Value::as_str),
            Some("already/set")
        );
    }

    #[test]
    fn decorate_tool_chunk_wraps_non_object_with_tool_name() {
        let value = json!("raw string");
        let out = decorate_tool_chunk("test/wrapper", &value);
        assert_eq!(
            out.get(TOOL_CHUNK_TOOL_NAME_KEY).and_then(Value::as_str),
            Some("test/wrapper")
        );
        assert_eq!(out.get("chunk"), Some(&value));
    }

    #[tokio::test]
    async fn sent_tool_stream_chunk_has_tool_name() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        scope_stream_yield(Some(tx), async {
            let value = json!({ "events": [{ "kind": "assistant_thinking", "thinking": "hi" }] });
            let decorated = decorate_tool_chunk("test/streaming_tool", &value);
            send_tool_stream_chunk(&decorated);
        })
        .await;
        let received = rx.try_recv().expect("one chunk");
        assert_eq!(
            received
                .get(TOOL_CHUNK_TOOL_NAME_KEY)
                .and_then(Value::as_str),
            Some("test/streaming_tool"),
            "tool stream chunk must be decorated with toolName"
        );
        assert!(received.get("events").is_some());
    }
}
