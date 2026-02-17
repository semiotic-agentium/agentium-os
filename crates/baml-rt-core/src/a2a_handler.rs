use crate::Result;
use crate::bus::BusStream;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;

/// Shared A2A request handling abstraction used by both transport and tools.
#[async_trait]
pub trait A2aRequestHandler: Send + Sync {
    /// Canonical streaming entrypoint for A2A handling.
    async fn handle_a2a_stream(&self, request: Value) -> Result<BusStream<Value>>;
}

/// Collects a stream at the callsite that needs buffered responses.
pub async fn collect_a2a_stream(mut stream: BusStream<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

/// Collects a stream until the provided predicate matches an item.
pub async fn collect_a2a_stream_until<F>(
    mut stream: BusStream<Value>,
    mut should_stop: F,
) -> Vec<Value>
where
    F: FnMut(&Value) -> bool,
{
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let stop = should_stop(&item);
        out.push(item);
        if stop {
            break;
        }
    }
    out
}
