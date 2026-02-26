use async_trait::async_trait;
use futures_util::StreamExt;

use crate::{
    Result,
    a2a_wire::{A2aStreamChunk, A2aWireRequest},
    bus::BusStream,
    deferred::DeferredHolder,
};

/// Shared A2A request handling abstraction used by both transport and tools.
#[async_trait]
pub trait A2aRequestHandler: Send + Sync {
    /// Canonical streaming entrypoint for A2A handling.
    async fn handle_a2a_stream(&self, request: A2aWireRequest)
    -> Result<BusStream<A2aStreamChunk>>;
}

#[async_trait]
impl A2aRequestHandler for DeferredHolder<dyn A2aRequestHandler> {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.get()?.handle_a2a_stream(request).await
    }
}

/// Collects a stream at the callsite that needs buffered responses.
pub async fn collect_a2a_stream(mut stream: BusStream<A2aStreamChunk>) -> Vec<A2aStreamChunk> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

/// Collects a stream until the provided predicate matches an item.
pub async fn collect_a2a_stream_until<F>(
    mut stream: BusStream<A2aStreamChunk>,
    mut should_stop: F,
) -> Vec<A2aStreamChunk>
where
    F: FnMut(&A2aStreamChunk) -> bool,
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
