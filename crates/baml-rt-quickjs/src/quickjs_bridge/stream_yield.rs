//! Task-local stream-yield sender so tool-session streaming outputs can be pushed
//! into the same channel as __baml_stream results (see docs/argument-sketch-stream-trace.md).

use serde_json::Value;
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

/// If a stream-yield sender is set (we're inside __baml_stream), send each chunk.
/// Called from execute_tool_session_plan when returning Value::Array(streaming_outputs).
pub(crate) fn send_tool_stream_chunks(chunks: &[Value]) {
    let _ = STREAM_YIELD_SENDER.try_with(|opt| {
        if let Some(sender) = opt {
            for v in chunks {
                let _ = sender.try_send(v.clone());
            }
        }
    });
}
