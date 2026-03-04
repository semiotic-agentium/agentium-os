//! Live stream session: per-request response channel for HTTP A2A message.sendStream.
//!
//! **Invariants (LS1–LS6)** — keep this design simple as features are added:
//!
//! | ID | Invariant | Notes |
//! |----|-----------|-------|
//! | **LS1** | **Session key consistency** | Map key = canonical form of request `(context_id, task_id?)`. Same form used when pushing WORKING chunks from effects. Registration key = push key. |
//! | **LS2** | **At most one session per key** | `stream_sessions` is a map; at most one entry per `(context_id, task_id?)` key. |
//! | **LS3** | **Push is best-effort, no session creation** | `push_working_to_session(context_id, chunk)` only sends if a session exists. Missing session ⇒ no-op. |
//! | **LS4** | **Chunk shape** | Values pushed are JSON-RPC–shaped chunks (e.g. `statusUpdate.status.state = TASK_STATE_WORKING`). Formatting is the pusher’s responsibility. |
//! | **LS5** | **Session lifecycle** | Session removed when `run_live_stream_session` exits. Push must tolerate "session not found". |
//! | **LS6** | **No relay-owned session state** | The relay (EffectSubscriber) does not hold the session map; it only calls the transport's push. |
//!
//! **Per-request response:** One stream = one request. Each turn is a `TurnInput` (request + exclusive `LiveResponseSender`). No broadcast for response; no markers or drain/skip.

use std::{collections::HashMap, fmt, sync::Arc};

use baml_rt_core::ids::{ContextId, TaskId};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

/// Opaque key for live stream session lookup.
///
/// Constructed from [`ContextId`] plus optional [`TaskId`] so task-scoped
/// streams (e.g. delegated internal A2A children) do not collide under the
/// same context when running concurrently.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LiveStreamSessionKey(String);

impl LiveStreamSessionKey {
    pub fn from_context_id(context_id: &ContextId) -> Self {
        Self(context_id.as_str().to_string())
    }

    pub fn from_context_and_task(context_id: &ContextId, task_id: Option<&TaskId>) -> Self {
        match task_id {
            Some(task_id) => Self(format!(
                "{context_id}::{task_id}",
                context_id = context_id.as_str(),
                task_id = task_id.as_str()
            )),
            None => Self::from_context_id(context_id),
        }
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
/// Kept even though not consumed yet so future middleware adapters can share a typed sink.
/// Allowed dead code: this trait is intentionally staged for upcoming middleware integration.
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

/// Single capability: push a raw chunk to the session's relay channel for this scope key.
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

    /// Push a raw chunk to the session's relay for this scope, if one exists (LS3). No-op if not found.
    /// The collect path drains this and emits as RelayChunk so tool/status chunks stay in order with message chunks.
    pub async fn push_relay_chunk(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
        chunk: Value,
    ) {
        let key = LiveStreamSessionKey::from_context_and_task(context_id, task_id);
        let tx = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(&key)
                .and_then(|s| s.relay_tx.clone())
                .or_else(|| {
                    // Backward-compatible fallback for message-scoped sessions keyed by context only.
                    sessions
                        .get(&LiveStreamSessionKey::from_context_id(context_id))
                        .and_then(|s| s.relay_tx.clone())
                })
        };
        if let Some(tx) = tx {
            // Best-effort delivery: never let a slow/blocked client stream backpressure runtime/tool execution.
            match tx.try_send(chunk) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    tracing::debug!(
                        context_id = %context_id,
                        "relay chunk dropped (channel full)"
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!("relay chunk send failed (receiver dropped)");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use baml_rt_core::ids::{ExternalId, TaskId};
    use serde_json::json;
    use tokio::sync::{Mutex, mpsc};

    use super::{LiveStreamSession, LiveStreamSessionKey, WorkingChunkPusher};

    #[tokio::test]
    async fn push_relay_chunk_prefers_task_scoped_session_key() {
        let context_id = baml_rt_core::ids::ContextId::new(5, 1);
        let task_id = TaskId::from_external(ExternalId::new("child-task-1".to_string()));

        let (ctx_tx, mut ctx_rx) = mpsc::channel(2);
        let (task_tx, mut task_rx) = mpsc::channel(2);
        let (turn_tx, _) = async_channel::unbounded();

        let sessions: Arc<Mutex<HashMap<_, _>>> = Arc::new(Mutex::new(HashMap::from([
            (
                LiveStreamSessionKey::from_context_id(&context_id),
                LiveStreamSession {
                    turn_tx: turn_tx.clone(),
                    relay_tx: Some(ctx_tx),
                },
            ),
            (
                LiveStreamSessionKey::from_context_and_task(&context_id, Some(&task_id)),
                LiveStreamSession {
                    turn_tx,
                    relay_tx: Some(task_tx),
                },
            ),
        ])));

        let pusher = WorkingChunkPusher::new(sessions);
        pusher
            .push_relay_chunk(&context_id, Some(&task_id), json!({"v": 1}))
            .await;

        let task_msg = task_rx.recv().await.expect("task-scoped relay chunk");
        assert_eq!(task_msg.get("v").and_then(|v| v.as_i64()), Some(1));
        assert!(ctx_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn push_relay_chunk_falls_back_to_context_session_key() {
        let context_id = baml_rt_core::ids::ContextId::new(9, 1);
        let task_id = TaskId::from_external(ExternalId::new("child-task-2".to_string()));

        let (ctx_tx, mut ctx_rx) = mpsc::channel(2);
        let (turn_tx, _) = async_channel::unbounded();

        let sessions: Arc<Mutex<HashMap<_, _>>> = Arc::new(Mutex::new(HashMap::from([(
            LiveStreamSessionKey::from_context_id(&context_id),
            LiveStreamSession {
                turn_tx,
                relay_tx: Some(ctx_tx),
            },
        )])));

        let pusher = WorkingChunkPusher::new(sessions);
        pusher
            .push_relay_chunk(&context_id, Some(&task_id), json!({"v": 2}))
            .await;

        let ctx_msg = ctx_rx.recv().await.expect("context-scoped relay chunk");
        assert_eq!(ctx_msg.get("v").and_then(|v| v.as_i64()), Some(2));
    }
}
