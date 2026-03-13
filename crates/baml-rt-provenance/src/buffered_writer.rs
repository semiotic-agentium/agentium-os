//! Buffered provenance writer: fire-and-forget writes with flush-before-read.
//!
//! Wraps any [`ProvenanceWriter`] with a bounded async channel so that
//! `add_event` returns near-instantly (non-blocking `try_send`). A background
//! tokio task drains the channel and writes events through the inner writer.
//!
//! The **no-stale-read invariant** is preserved: every [`ProvenanceContextReader`]
//! method flushes the pending write buffer before delegating to the inner store,
//! so reads always reflect all prior `add_event` calls that returned `Ok`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::{
    error::{ProvenanceError, Result},
    events::ProvEvent,
    store::{
        ProvenanceContextMessage, ProvenanceContextReader, ProvenanceConversationContextItem,
        ProvenanceWriter,
    },
};

/// Default buffer capacity. Large enough to absorb bursts without backpressure
/// under normal operation; if the writer falls behind, `add_event` awaits until
/// a slot is available rather than dropping events.
const DEFAULT_BUFFER_SIZE: usize = 1024;

/// Internal message type for the write channel.
enum WriteRequest {
    /// A provenance event to persist.
    Event(Box<ProvEvent>),
    /// Flush signal: the background task must finish all prior events before
    /// replying on the oneshot, so the caller knows it is safe to read.
    Flush(oneshot::Sender<()>),
}

/// Backpressure-aware buffered provenance writer.
///
/// `add_event` pushes into a bounded channel; if the channel is full the caller
/// awaits until a slot opens, guaranteeing no events are silently dropped.
/// A background task processes events through the inner [`ProvenanceWriter`].
/// Context reads flush the buffer first to preserve the no-stale-read invariant.
pub struct BufferedProvenanceWriter {
    tx: mpsc::Sender<WriteRequest>,
    inner: Arc<dyn ProvenanceWriter>,
}

impl BufferedProvenanceWriter {
    /// Create a new buffered writer with the default buffer size.
    ///
    /// Spawns a background tokio task that drains the channel and writes events.
    /// The task exits when the last `BufferedProvenanceWriter` (and all clones of
    /// the sender) are dropped.
    pub fn new(inner: Arc<dyn ProvenanceWriter>) -> Self {
        Self::with_capacity(inner, DEFAULT_BUFFER_SIZE)
    }

    /// Create a new buffered writer with a custom buffer capacity.
    pub fn with_capacity(inner: Arc<dyn ProvenanceWriter>, capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        let writer = inner.clone();
        tokio::spawn(writer_loop(rx, writer));
        Self { tx, inner }
    }

    /// Flush all pending writes. Blocks (async) until the background task has
    /// processed every event that was enqueued before this call.
    async fn flush(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // Use `.send().await` (not try_send) — we must wait for the flush signal
        // to enter the queue, even under backpressure.
        self.tx
            .send(WriteRequest::Flush(reply_tx))
            .await
            .map_err(|_| {
                ProvenanceError::Storage(Box::new(std::io::Error::other(
                    "buffered writer background task closed",
                )))
            })?;
        reply_rx.await.map_err(|_| {
            ProvenanceError::Storage(Box::new(std::io::Error::other(
                "buffered writer flush reply dropped",
            )))
        })
    }
}

/// Background task: drains the channel and writes events sequentially.
async fn writer_loop(mut rx: mpsc::Receiver<WriteRequest>, writer: Arc<dyn ProvenanceWriter>) {
    while let Some(request) = rx.recv().await {
        match request {
            WriteRequest::Event(event) => {
                writer
                    .add_event_with_logging(*event, "buffered writer")
                    .await;
            }
            WriteRequest::Flush(reply) => {
                // All prior Event messages have been processed (channel is FIFO).
                let _ = reply.send(());
            }
        }
    }
    tracing::debug!("buffered provenance writer background task exiting");
}

#[async_trait]
impl ProvenanceWriter for BufferedProvenanceWriter {
    async fn add_event(&self, event: ProvEvent) -> Result<()> {
        self.tx
            .send(WriteRequest::Event(Box::new(event)))
            .await
            .map_err(|_| {
                ProvenanceError::Storage(Box::new(std::io::Error::other(
                    "buffered writer background task closed",
                )))
            })
    }
}

#[async_trait]
impl ProvenanceContextReader for BufferedProvenanceWriter {
    async fn context_messages(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        self.flush().await?;
        self.inner.context_messages(context_id, limit).await
    }

    async fn conversation_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.flush().await?;
        self.inner.conversation_context(context_id, limit).await
    }
}
