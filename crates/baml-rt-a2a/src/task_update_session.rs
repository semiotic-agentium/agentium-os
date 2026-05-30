// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Replay-then-live session over a [`TaskUpdateBroadcaster`].
//!
//! ## Why this exists
//!
//! [`TaskUpdateBroadcaster`] is a thin shell around
//! `tokio::sync::broadcast`: it carries frames between the writer that
//! committed a [`baml_rt_provenance::ProvEvent`] and any live subscribers
//! attached at the moment of commit. There are two failure modes the bare
//! receiver does not handle:
//!
//! 1. **Reconnect / late subscribe**: the subscriber missed every frame
//!    committed before its `subscribe()` call. The provenance graph is
//!    durable, so the missed frames are recoverable; the session replays
//!    them up to its `subscribe()` cursor before delivering the live tail.
//! 2. **Lag**: a subscriber that drains slower than the producer fills
//!    `capacity` frames receives `RecvError::Lagged(n)` instead of the
//!    next frame. Plain `broadcast::Receiver` consumers would silently
//!    drop those frames; the session falls back to graph replay from the
//!    last successfully delivered replay cursor and resumes live
//!    delivery transparently.
//!
//! ## Cluster note
//!
//! The broadcast leg is single-process. In a multi-pod runner deployment
//! a subscriber attached to pod B will not see live frames produced by pod
//! A; the replay path covers that gap by reading from the shared
//! provenance graph. This is the deliberate durability / latency
//! trade-off documented in `docs/baml-rt-conversation-spec.md`.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_provenance::{ReplayError, TaskReplayCursor, metamodel::ScopedTaskRef};
use futures_util::stream::BoxStream;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::task_update_broadcaster::{TaskStreamKey, TaskUpdateBroadcaster, TaskUpdateFrame};

/// Pluggable replay backend. [`TaskGraphReader`] provides the production
/// implementation by translating `replay_since` into typed
/// `GraphQuery` / `EdgeProjection` reads against the provenance graph;
/// keeping the trait local to this module lets the broadcaster + session
/// layer stay unit-testable with a mock replay source.
///
/// Implementors must order frames monotonically by
/// [`TaskReplayCursor`]. The session relies on this for lag recovery.
#[async_trait]
pub trait TaskUpdateReplaySource: Send + Sync + 'static {
    /// Yield every frame for `scoped` whose cursor is strictly greater
    /// than `since`. `since = None` means "from the start of the task".
    /// Errors that occur partway through the stream surface inside the
    /// stream's `Item` slot; the outer `Result` only fails when the
    /// stream itself cannot be opened.
    async fn replay_since(
        &self,
        scoped: ScopedTaskRef,
        since: Option<TaskReplayCursor>,
    ) -> Result<BoxStream<'static, Result<TaskUpdateFrame, ReplayError>>, ReplayError>;
}

/// Failures returned by [`TaskUpdateSession::open`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenSessionError {
    /// The replay source could not be opened (typically a database
    /// connectivity error).
    #[error(transparent)]
    Replay(#[from] ReplayError),
}

/// One subscriber's view of a single task's update stream. Drains
/// any backlog from the durable graph first, then falls through to live
/// frames; on broadcast lag, transparently re-replays from the last
/// delivered cursor and resumes live.
///
/// Construction order inside [`Self::open`] is not negotiable: the
/// broadcast subscription **must** be acquired before the replay stream
/// starts pulling, so any frame the writer commits during replay is
/// either:
///
/// - already visible in the graph (delivered through the replay stream), or
/// - buffered in the broadcast channel for later live delivery.
///
/// Without this ordering a frame committed in the gap window would be
/// invisible to the subscriber.
pub struct TaskUpdateSession {
    receiver: broadcast::Receiver<TaskUpdateFrame>,
    state: SessionState,
    last_cursor: Option<TaskReplayCursor>,
    source: Arc<dyn TaskUpdateReplaySource>,
    scoped: ScopedTaskRef,
}

enum SessionState {
    /// Draining the replay stream; advances to `Live` when the stream
    /// returns `None`.
    Replaying(BoxStream<'static, Result<TaskUpdateFrame, ReplayError>>),
    /// Reading from the broadcast channel; advances to `Replaying` on
    /// `RecvError::Lagged` and to `Closed` on `RecvError::Closed`.
    Live,
    /// Terminal — the underlying channel is closed; `next()` returns
    /// `None` from this state forever.
    Closed,
}

impl std::fmt::Debug for TaskUpdateSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskUpdateSession")
            .field(
                "state",
                match self.state {
                    SessionState::Replaying(_) => &"Replaying",
                    SessionState::Live => &"Live",
                    SessionState::Closed => &"Closed",
                },
            )
            .field("last_cursor", &self.last_cursor)
            .finish()
    }
}

impl TaskUpdateSession {
    /// Open a session for `scoped`, optionally backfilling from
    /// `since`.
    ///
    /// 1. Subscribe to the broadcast first (no live frames are missed
    ///    during replay).
    /// 2. Open the replay stream from `since`.
    pub async fn open(
        broadcaster: &TaskUpdateBroadcaster,
        source: Arc<dyn TaskUpdateReplaySource>,
        scoped: ScopedTaskRef,
        since: Option<TaskReplayCursor>,
    ) -> Result<Self, OpenSessionError> {
        let key = TaskStreamKey::new(
            // ScopedTaskRef does not currently expose its ContextId / TaskId
            // through accessors, but the underlying NodeId values are public.
            // We round-trip through the on-disk format because
            // `ScopedTaskRef` does not yet expose the wire ids directly.
            stream_key_ctx_from_scoped(&scoped),
            stream_key_task_from_scoped(&scoped),
        );
        let receiver = broadcaster.subscribe(&key);
        let stream = source.replay_since(scoped.clone(), since.clone()).await?;
        Ok(Self {
            receiver,
            state: SessionState::Replaying(stream),
            last_cursor: since,
            source,
            scoped,
        })
    }

    /// Cursor of the most recently delivered frame, if any. Subscribers
    /// that disconnect can pass this back to a future
    /// [`Self::open`] call as the `since` cursor.
    pub fn last_cursor(&self) -> Option<&TaskReplayCursor> {
        self.last_cursor.as_ref()
    }

    /// Yield the next frame.
    ///
    /// Returns `Ok(None)` when the underlying broadcast channel has been
    /// retired (terminal task state). `Ok(Some(frame))` advances the
    /// internal cursor to `frame.cursor()`. Lag is handled internally —
    /// callers never observe `RecvError::Lagged`.
    pub async fn next(&mut self) -> Result<Option<TaskUpdateFrame>, ReplayError> {
        loop {
            match &mut self.state {
                SessionState::Replaying(stream) => {
                    match futures_util::StreamExt::next(stream).await {
                        Some(Ok(frame)) => {
                            self.last_cursor = Some(frame.cursor().clone());
                            return Ok(Some(frame));
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            self.state = SessionState::Live;
                            continue;
                        }
                    }
                }
                SessionState::Live => match self.receiver.recv().await {
                    Ok(frame) => {
                        // Skip any live frames the replay stream already
                        // delivered (the broadcast subscription was set
                        // up before replay; the overlap window is the
                        // burst between subscribe() and the first
                        // post-subscribe replay row).
                        if let Some(last) = self.last_cursor.as_ref()
                            && frame.cursor() <= last
                        {
                            continue;
                        }
                        self.last_cursor = Some(frame.cursor().clone());
                        return Ok(Some(frame));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        self.state = SessionState::Closed;
                        return Ok(None);
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            skipped,
                            scoped = ?self.scoped,
                            last_cursor = ?self.last_cursor,
                            "TaskUpdateSession: live receiver lagged; falling back to graph replay"
                        );
                        let stream = self
                            .source
                            .replay_since(self.scoped.clone(), self.last_cursor.clone())
                            .await?;
                        self.state = SessionState::Replaying(stream);
                        continue;
                    }
                },
                SessionState::Closed => return Ok(None),
            }
        }
    }

    /// Discard the replay-then-live state machine and surface the raw
    /// broadcast receiver. Intended for callers that have already
    /// drained replay (e.g. via repeated [`Self::next`] until the
    /// session reports it is in the live state) and do not require
    /// lag-recovery — typically the SSE proxy where the client itself
    /// is expected to reconnect on lag.
    ///
    /// Consumes `self` so the typed lag-recovery contract cannot
    /// accidentally be re-used after handoff.
    pub fn into_live(self) -> broadcast::Receiver<TaskUpdateFrame> {
        self.receiver
    }

    /// Drain the replay leg only — pull frames until the session would
    /// transition into live mode, then return everything seen. Equivalent
    /// to the historical "what's been committed since the cursor"
    /// contract; cheaper than `next()` past
    /// replay because it never blocks on the broadcast channel.
    ///
    /// Used by the SSE handler in the moment between
    /// `tasks/subscribe` accepting the request and the live receiver
    /// being polled.
    pub async fn drain_replay(&mut self) -> Result<Vec<TaskUpdateFrame>, ReplayError> {
        let mut out = Vec::new();
        loop {
            // Replaying state pulls from the stream; once it returns
            // None the session transitions to Live and `next()` would
            // block on the broadcast receiver. We stop right there.
            let prev_in_replay = matches!(self.state, SessionState::Replaying(_));
            if !prev_in_replay {
                return Ok(out);
            }
            match &mut self.state {
                SessionState::Replaying(stream) => {
                    match futures_util::StreamExt::next(stream).await {
                        Some(Ok(frame)) => {
                            self.last_cursor = Some(frame.cursor().clone());
                            out.push(frame);
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            self.state = SessionState::Live;
                            return Ok(out);
                        }
                    }
                }
                SessionState::Live | SessionState::Closed => return Ok(out),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ScopedTaskRef → TaskStreamKey adapter.
// ---------------------------------------------------------------------------

fn stream_key_ctx_from_scoped(scoped: &ScopedTaskRef) -> baml_rt_core::ids::ContextId {
    scoped.context_id()
}

fn stream_key_task_from_scoped(scoped: &ScopedTaskRef) -> baml_rt_core::ids::TaskId {
    scoped.task_id()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_stream::stream;
    use baml_rt_core::ids::{ActivityAnchorId, ArtifactId, ContextId, ExternalId, TaskId};
    use baml_rt_provenance::metamodel::{ContextNodeId, TaskNodeId};
    use tokio::sync::Mutex;

    use super::*;
    use crate::task_update_broadcaster::{ArtifactRef, TaskUpdateBroadcaster, TaskUpdateFrame};

    fn anchor(n: u64) -> ActivityAnchorId {
        ActivityAnchorId::from_counter(n)
    }

    fn cursor(n: u64) -> TaskReplayCursor {
        TaskReplayCursor::from_anchor(anchor(n)).expect("cursor")
    }

    fn artifact_frame(n: u64) -> TaskUpdateFrame {
        TaskUpdateFrame::ArtifactGenerated {
            artifact: ArtifactRef {
                task_id: TaskId::from_external(ExternalId::new("t-session")),
                artifact_id: Some(ArtifactId::from_external(ExternalId::new(format!("a-{n}")))),
                artifact_type: None,
            },
            cursor: cursor(n),
        }
    }

    fn scoped_for_test() -> ScopedTaskRef {
        let ctx = ContextId::new(99, 0);
        let task = TaskId::from_external(ExternalId::new("t-session"));
        ScopedTaskRef::new_for_test(
            ContextNodeId::for_context_id(&ctx),
            TaskNodeId::for_task_id(&task),
        )
    }

    /// Replay backend that hands out a fixed list of frames for the
    /// first call, then an empty list (or alternative payload) for
    /// subsequent calls. Lets tests prove lag-recovery re-runs replay.
    struct ScriptedSource {
        scripted: Mutex<Vec<Vec<TaskUpdateFrame>>>,
    }

    impl ScriptedSource {
        fn new(scripts: Vec<Vec<TaskUpdateFrame>>) -> Arc<Self> {
            Arc::new(Self {
                scripted: Mutex::new(scripts),
            })
        }
    }

    #[async_trait]
    impl TaskUpdateReplaySource for ScriptedSource {
        async fn replay_since(
            &self,
            _scoped: ScopedTaskRef,
            _since: Option<TaskReplayCursor>,
        ) -> Result<BoxStream<'static, Result<TaskUpdateFrame, ReplayError>>, ReplayError> {
            let next = {
                let mut g = self.scripted.lock().await;
                if g.is_empty() {
                    Vec::new()
                } else {
                    g.remove(0)
                }
            };
            let s = stream! {
                for f in next {
                    yield Ok(f);
                }
            };
            Ok(Box::pin(s))
        }
    }

    #[tokio::test]
    async fn session_drains_replay_then_switches_to_live() {
        let bc = TaskUpdateBroadcaster::default();
        let source = ScriptedSource::new(vec![vec![artifact_frame(1), artifact_frame(2)]]);
        let scoped = scoped_for_test();
        let mut session = TaskUpdateSession::open(&bc, source, scoped.clone(), None)
            .await
            .expect("open session");

        // Drain replay.
        let f1 = session.next().await.expect("next 1").expect("some 1");
        assert_eq!(f1.cursor(), &cursor(1));
        let f2 = session.next().await.expect("next 2").expect("some 2");
        assert_eq!(f2.cursor(), &cursor(2));

        // Now in live state. Send a fresh frame from another task and
        // expect it to be delivered.
        let key = TaskStreamKey::new(
            stream_key_ctx_from_scoped(&scoped),
            stream_key_task_from_scoped(&scoped),
        );
        let writer = bc.writer(key);
        let _ = writer.send(artifact_frame(3));

        let f3 = tokio::time::timeout(Duration::from_millis(200), session.next())
            .await
            .expect("recv within deadline")
            .expect("ok")
            .expect("some 3");
        assert_eq!(f3.cursor(), &cursor(3));
    }

    #[tokio::test]
    async fn session_returns_none_when_channel_retired() {
        let bc = TaskUpdateBroadcaster::default();
        let source = ScriptedSource::new(vec![vec![]]);
        let scoped = scoped_for_test();
        let mut session = TaskUpdateSession::open(&bc, source, scoped.clone(), None)
            .await
            .expect("open session");

        let key = TaskStreamKey::new(
            stream_key_ctx_from_scoped(&scoped),
            stream_key_task_from_scoped(&scoped),
        );
        let writer = bc.writer(key);
        writer.retire_task();

        let res = tokio::time::timeout(Duration::from_millis(200), session.next())
            .await
            .expect("recv within deadline")
            .expect("no error");
        assert!(
            res.is_none(),
            "expected None when broadcaster retired the channel"
        );
    }

    #[tokio::test]
    async fn session_recovers_from_lag_via_replay_fallback() {
        // Capacity 4, then push 8 frames before the consumer reads any —
        // forces RecvError::Lagged on the next live recv.
        let bc = TaskUpdateBroadcaster::with_capacity(4);
        let scoped = scoped_for_test();
        let key = TaskStreamKey::new(
            stream_key_ctx_from_scoped(&scoped),
            stream_key_task_from_scoped(&scoped),
        );
        // Replay #1: empty (caller has no backlog).
        // Replay #2 (after lag): the 8 frames the consumer dropped.
        let lagged_replay: Vec<TaskUpdateFrame> = (10..18).map(artifact_frame).collect();
        let source = ScriptedSource::new(vec![vec![], lagged_replay.clone()]);

        let mut session = TaskUpdateSession::open(&bc, source, scoped, None)
            .await
            .expect("open session");
        let writer = bc.writer(key);
        for n in 10..18 {
            let _ = writer.send(artifact_frame(n));
        }

        // The first `next()` will see Lagged on the live receiver and
        // re-enter replay; we then drain the 8 replayed frames.
        let mut drained: Vec<u64> = Vec::with_capacity(8);
        for _ in 0..8 {
            let f = tokio::time::timeout(Duration::from_millis(200), session.next())
                .await
                .expect("recv within deadline")
                .expect("ok")
                .expect("some");
            let idx = lagged_replay
                .iter()
                .position(|x| x.cursor() == f.cursor())
                .expect("replay frame in expected set");
            drained.push(10 + idx as u64);
        }
        drained.sort();
        assert_eq!(
            drained,
            (10..18).collect::<Vec<_>>(),
            "no frame dropped after lag"
        );
    }
}
