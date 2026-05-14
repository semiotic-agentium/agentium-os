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

/// Marker sub-trait: full A2A **chat parity** (HTTP-equivalent QuickJS wiring).
///
/// Only types that always build a [`baml_rt_quickjs::QuickJSBridge`] through
/// [`baml_rt_quickjs::QuickJSBridge::register_baml_functions`] should implement this.
/// That path verifies [`baml_rt_quickjs::a2a_chat_surface::A2A_CHAT_HOST_GLOBALS`] at the end
/// of registration, so the machine-spirit cannot drift silently.
///
/// Use this trait as a **type constraint** for in-memory test clients when
/// execution-session + step-executor behaviour must match the HTTP host — not as a dynamic
/// runtime probe (QuickJS eval futures are not `Send`-safe across `async_trait` boundaries).
pub trait A2aJsChatHost: A2aRequestHandler {}

#[async_trait]
impl A2aRequestHandler for DeferredHolder<dyn A2aJsChatHost> {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.get()?.handle_a2a_stream(request).await
    }
}

impl A2aJsChatHost for DeferredHolder<dyn A2aJsChatHost> {}

/// Explicit one-shot/test boundary: collect a stream into memory.
///
/// Use only where buffering is the contract (e.g. stdio compatibility or tests). Live HTTP/SSE
/// forwarding must preserve the stream and must not call this helper.
pub async fn collect_a2a_stream_one_shot(
    mut stream: BusStream<A2aStreamChunk>,
) -> Vec<A2aStreamChunk> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

/// Explicit one-shot/test boundary: collect until a predicate matches.
pub async fn collect_a2a_stream_until_one_shot<F>(
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
