//! Live stream session: per-request response channel for HTTP A2A message.sendStream.
//!
//! **Invariants (LS1–LS6)** — keep this design simple as features are added:
//!
//! | ID | Invariant | Notes |
//! |----|-----------|-------|
//! | **LS1** | **Session key consistency** | Map key = canonical form of request `context_id` (`context_id.as_str()`). Same form used when pushing WORKING chunks from effects. Registration key = push key. |
//! | **LS2** | **At most one session per context** | `stream_sessions` is a map; at most one entry per key. |
//! | **LS3** | **Push is best-effort, no session creation** | `push_working_to_session(context_id, chunk)` only sends if a session exists. Missing session ⇒ no-op. |
//! | **LS4** | **Chunk shape** | Values pushed are JSON-RPC–shaped chunks (e.g. `statusUpdate.status.state = TASK_STATE_WORKING`). Formatting is the pusher’s responsibility. |
//! | **LS5** | **Session lifecycle** | Session removed when `run_live_stream_session` exits. Push must tolerate "session not found". |
//! | **LS6** | **No relay-owned session state** | The relay (EffectSubscriber) does not hold the session map; it only calls the transport's push. |
//!
//! **Per-request response:** One stream = one request. Each turn is a `TurnInput` (request + exclusive `LiveResponseSender`). No broadcast for response; no markers or drain/skip.

use std::{collections::HashMap, fmt, sync::Arc};

use baml_rt_core::ids::ContextId;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

/// Opaque key for live stream session lookup.
///
/// Constructed only from [`ContextId`] so that the same key is used when
/// registering a session (from the request) and when pushing WORKING chunks
/// (from the effect's context_id). No fallback or string normalization:
/// matching is by this key only.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LiveStreamSessionKey(String);

impl LiveStreamSessionKey {
    pub fn from_context_id(context_id: &ContextId) -> Self {
        Self(context_id.as_str().to_string())
    }
}

impl fmt::Display for LiveStreamSessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A single chunk on a live response stream (one request).
/// Distinguishes from raw `Value` and from WORKING push at the type level.
#[derive(Clone, Debug)]
pub struct LiveResponseChunk(pub Value);

/// Exclusive sender for one request's response stream.
/// Dropping closes the stream for the client.
#[derive(Clone)]
pub struct LiveResponseSender(pub async_channel::Sender<LiveResponseChunk>);

impl LiveResponseSender {
    pub fn new(sender: async_channel::Sender<LiveResponseChunk>) -> Self {
        Self(sender)
    }

    pub async fn send(
        &self,
        chunk: LiveResponseChunk,
    ) -> Result<(), async_channel::SendError<LiveResponseChunk>> {
        self.0.send(chunk).await
    }
}

/// Contract for "send chunk for this turn". Reserved for loop/middleware abstraction.
#[allow(dead_code)]
#[allow(async_fn_in_trait)]
pub trait TurnResponseSink: Send + Sync {
    /// Send one chunk to the client for this turn.
    fn send_chunk(
        &self,
        chunk: LiveResponseChunk,
    ) -> impl std::future::Future<Output = Result<(), async_channel::SendError<LiveResponseChunk>>> + Send;
}

impl TurnResponseSink for LiveResponseSender {
    async fn send_chunk(
        &self,
        chunk: LiveResponseChunk,
    ) -> Result<(), async_channel::SendError<LiveResponseChunk>> {
        self.0.send(chunk).await
    }
}

/// One turn: the request and the exclusive sink for this turn's response.
/// The loop receives only this; no broadcast, no turn identity in the stream.
#[derive(Clone)]
pub struct TurnInput {
    pub request: Value,
    pub response_tx: LiveResponseSender,
}

/// One live stream session: turns are sent here.
#[derive(Clone)]
pub struct LiveStreamSession {
    /// Sends (request, response_sink) to the loop. One TurnInput per request (spawn or attach).
    pub turn_tx: async_channel::Sender<TurnInput>,
    /// Optional: relay pushes raw chunks here; collect path drains and emits in order (single stream). Set when stream starts, cleared when stream ends.
    pub relay_tx: Option<mpsc::Sender<Value>>,
}

/// Single capability: push a raw chunk to the session's relay channel for this context.
///
/// Transport owns the session map and sets relay_tx when the stream starts; relay holds an `Arc`
/// and calls `push_relay_chunk` from effect handlers. Chunks are drained by the collect path
/// so delivery order is preserved (single stream).
pub struct WorkingChunkPusher {
    sessions: Arc<Mutex<HashMap<LiveStreamSessionKey, LiveStreamSession>>>,
}

impl WorkingChunkPusher {
    pub fn new(sessions: Arc<Mutex<HashMap<LiveStreamSessionKey, LiveStreamSession>>>) -> Self {
        Self { sessions }
    }

    /// Push a raw chunk to the session's relay for this context, if one exists (LS3). No-op if not found.
    /// The collect path drains this and emits as RelayChunk so tool/status chunks stay in order with message chunks.
    pub async fn push_relay_chunk(&self, context_id: &ContextId, chunk: Value) {
        let key = LiveStreamSessionKey::from_context_id(context_id);
        let tx = {
            let sessions = self.sessions.lock().await;
            sessions.get(&key).and_then(|s| s.relay_tx.clone())
        };
        if let Some(tx) = tx
            && tx.send(chunk).await.is_err()
        {
            tracing::debug!("relay chunk send failed (receiver dropped)");
        }
    }
}
