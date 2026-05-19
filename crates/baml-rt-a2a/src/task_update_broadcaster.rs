//! In-memory broadcast for live A2A task updates.
//!
//! Replaces the durable `a2a_update` SSE replay queue with a per-`(ContextId,
//! TaskId)` `tokio::sync::broadcast` channel. The provenance graph remains
//! the durable source of truth (replay-since reads from there); this
//! broadcaster only carries frames between the writer that committed them
//! and any live SSE subscribers attached at the moment of commit.
//!
//! ## Boundary contract
//!
//! - **Writer side**: code that just committed a [`ProvEvent`] to the
//!   provenance graph mints a [`TaskUpdateFrame`] and sends it through a
//!   [`TaskStreamWriter`] handle obtained from
//!   [`TaskUpdateBroadcaster::writer`]. The handle is RAII; dropping it
//!   does **not** remove the channel by itself (multiple writers may
//!   share the same `(ctx, task)` over the lifetime of the task).
//!   [`TaskStreamWriter::retire_task`] is the explicit terminal-cleanup
//!   call invoked on COMPLETED / FAILED / CANCELED transitions; it
//!   removes the channel from the broadcaster and drops the underlying
//!   sender (live subscribers observe `RecvError::Closed` on the next
//!   poll).
//! - **Subscriber side**: SSE handlers call
//!   [`TaskUpdateBroadcaster::subscribe`] before invoking the
//!   replay-from-graph stage, so no live frame committed during replay
//!   is missed (the channel is created on first reference).
//!
//! ## Sharding
//!
//! The shard count is `16` and is selected by the lower bits of the
//! key's `Hash`. With `O(num_subscribers)` work per send the lock
//! contention is dominated by the `DashMap` shard lock during the
//! `entry`/`remove` transitions, not by the broadcast send itself; 16
//! shards is more than enough headroom for the design-partner task fan-out
//! ceiling. The constant lives at module scope so the test harness can
//! reference it without recomputing.
//!
//! ## Capacity
//!
//! Default channel capacity is `256` frames. A subscriber that lags more
//! than `capacity` frames behind the producer receives
//! [`tokio::sync::broadcast::error::RecvError::Lagged`] on the next poll;
//! the [`TaskUpdateSession`](super) state machine planned for Phase B2
//! will translate this into a fall-back replay-from-graph from the last
//! successfully delivered [`ActivityAnchorId`].

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{Arc, Weak},
};

use baml_rt_core::ids::{ContextId, TaskId};
// Frame + reference types live in the provenance crate (see
// `baml-rt-provenance::task_graph_reader`) so the typed payloads
// (`A2ATaskStateProps`) and graph-only metadata (`MessageRef` /
// `ArtifactRef`) stay in one place. Re-export so existing
// `crate::task_update_broadcaster::TaskUpdateFrame` imports keep
// working after Phase C.
pub use baml_rt_provenance::{ArtifactRef, MessageRef, TaskUpdateFrame};
use dashmap::DashMap;
use tokio::sync::broadcast;

/// Default per-channel ring buffer capacity in frames. Sized for typical
/// SSE subscriber drift; tunable via [`TaskUpdateBroadcaster::with_capacity`].
pub const DEFAULT_CAPACITY: usize = 256;

/// Number of `DashMap` shards. The shard for a key is selected by the
/// lower bits of the key's `Hash`. Power of two so the modulus folds to a
/// mask in the optimiser.
pub const SHARD_COUNT: usize = 16;

/// Composite key for one task's live update channel. `ContextId` +
/// `TaskId` is the same uniqueness boundary the provenance graph uses for
/// `Task` nodes, so two different contexts that happen to reuse the same
/// task id never collide on the same channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskStreamKey {
    ctx: ContextId,
    task: TaskId,
}

impl TaskStreamKey {
    pub fn new(ctx: ContextId, task: TaskId) -> Self {
        Self { ctx, task }
    }

    pub fn ctx(&self) -> &ContextId {
        &self.ctx
    }

    pub fn task(&self) -> &TaskId {
        &self.task
    }

    fn shard_index(&self) -> usize {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        (h.finish() as usize) & (SHARD_COUNT - 1)
    }
}

/// Inner state of [`TaskUpdateBroadcaster`]. Held inside an `Arc` so
/// [`TaskStreamWriter`] can keep a `Weak` back-reference for self-removal
/// without forcing the broadcaster to outlive its writers.
struct Inner {
    shards: [DashMap<TaskStreamKey, broadcast::Sender<TaskUpdateFrame>>; SHARD_COUNT],
    capacity: usize,
}

impl Inner {
    fn shard_for(
        &self,
        key: &TaskStreamKey,
    ) -> &DashMap<TaskStreamKey, broadcast::Sender<TaskUpdateFrame>> {
        &self.shards[key.shard_index()]
    }

    fn ensure_sender(&self, key: &TaskStreamKey) -> broadcast::Sender<TaskUpdateFrame> {
        let shard = self.shard_for(key);
        if let Some(existing) = shard.get(key) {
            return existing.clone();
        }
        // `entry().or_insert_with` collapses the read-then-insert race
        // with concurrent `subscribe` / `writer` callers without having
        // to hold the shard lock across the broadcast::channel allocation
        // for the common (already-present) case above.
        let entry = shard
            .entry(key.clone())
            .or_insert_with(|| broadcast::channel(self.capacity).0);
        entry.value().clone()
    }

    fn retire(&self, key: &TaskStreamKey) {
        // `remove` drops the held `Sender`; outstanding receivers see
        // `RecvError::Closed` on their next `recv()`. Idempotent: a
        // second retire on the same key is a no-op.
        let _ = self.shard_for(key).remove(key);
    }
}

/// Process-wide registry of in-memory live broadcast channels keyed by
/// [`TaskStreamKey`]. Inexpensive to clone — the underlying state is
/// shared via `Arc`.
#[derive(Clone)]
pub struct TaskUpdateBroadcaster {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TaskUpdateBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let approx_open: usize = self.inner.shards.iter().map(DashMap::len).sum();
        f.debug_struct("TaskUpdateBroadcaster")
            .field("capacity", &self.inner.capacity)
            .field("shard_count", &SHARD_COUNT)
            .field("approx_open_channels", &approx_open)
            .finish()
    }
}

impl Default for TaskUpdateBroadcaster {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl TaskUpdateBroadcaster {
    /// New broadcaster with a custom per-channel capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // `DashMap` is not `Copy`, so we cannot use `[expr; N]`. Build
        // the array explicitly via `from_fn` so each shard is a fresh
        // map.
        let shards: [DashMap<TaskStreamKey, broadcast::Sender<TaskUpdateFrame>>; SHARD_COUNT] =
            std::array::from_fn(|_| DashMap::new());
        Self {
            inner: Arc::new(Inner { shards, capacity }),
        }
    }

    /// Per-channel capacity in frames.
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Subscribe to live frames for the given key. The channel is
    /// materialised lazily on first reference; subsequent subscribers
    /// share the same channel. Frames sent before this call returns are
    /// **not** replayed (see `TaskUpdateSession::open` in Phase B2 for
    /// the replay-then-live transition).
    pub fn subscribe(&self, key: &TaskStreamKey) -> broadcast::Receiver<TaskUpdateFrame> {
        self.inner.ensure_sender(key).subscribe()
    }

    /// Acquire a writer handle for the given key. The handle is RAII but
    /// **does not remove the channel on drop** — multiple writers may
    /// share the same `(ctx, task)` over the task lifetime. Use
    /// [`TaskStreamWriter::retire_task`] to remove the channel on
    /// terminal status transitions.
    pub fn writer(&self, key: TaskStreamKey) -> TaskStreamWriter {
        let sender = self.inner.ensure_sender(&key);
        TaskStreamWriter {
            inner: Arc::downgrade(&self.inner),
            key,
            sender,
        }
    }

    /// Retire (remove) a channel without holding a writer handle. Used
    /// by the cancel path where status updates flow through a different
    /// recorder. Idempotent.
    pub fn retire(&self, key: &TaskStreamKey) {
        self.inner.retire(key);
    }
}

/// Send-side handle to one task's live broadcast channel. RAII: the
/// handle keeps the channel alive only via its `Sender` clone; dropping
/// it does **not** remove the registry entry, because multiple
/// independent writers may emit to the same `(ctx, task)` over the task
/// lifetime (e.g. status updates and artifact updates flow through
/// different code paths). Removal is the explicit
/// [`TaskStreamWriter::retire_task`] call invoked on terminal status
/// transitions.
#[must_use = "the writer handle does nothing on its own; call .send(frame) or .retire_task()"]
pub struct TaskStreamWriter {
    inner: Weak<Inner>,
    key: TaskStreamKey,
    sender: broadcast::Sender<TaskUpdateFrame>,
}

impl std::fmt::Debug for TaskStreamWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskStreamWriter")
            .field("key", &self.key)
            .field("receiver_count", &self.sender.receiver_count())
            .finish()
    }
}

impl TaskStreamWriter {
    /// Key this writer is bound to.
    pub fn key(&self) -> &TaskStreamKey {
        &self.key
    }

    /// Number of live receivers currently subscribed. Useful for
    /// metrics + the "no subscriber, skip serialisation" optimisation.
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Send a frame to all live subscribers. Returns the number of
    /// receivers that observed the frame (`Ok(0)` is a normal "no live
    /// subscribers" case, not an error). `Err` is returned only if there
    /// are zero receivers; callers that care about the no-subscriber
    /// case should call [`Self::receiver_count`] first.
    pub fn send(
        &self,
        frame: TaskUpdateFrame,
    ) -> Result<usize, Box<broadcast::error::SendError<TaskUpdateFrame>>> {
        self.sender.send(frame).map_err(Box::new)
    }

    /// Remove the channel for this key from the broadcaster. Idempotent
    /// — calling it on a key that has already been retired is a no-op.
    /// Outstanding receivers observe `RecvError::Closed` on the next
    /// `recv()`; new subscribers after this call create a fresh channel.
    /// Consumes `self` to make "writer is no longer usable" structural.
    pub fn retire_task(self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.retire(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use baml_rt_core::ids::{ActivityAnchorId, ContextId, ExternalId, TaskId};
    use baml_rt_provenance::metamodel::{A2ATaskStateProps, TaskNodeId, TaskStatusKind};

    use super::*;

    fn key(seed: &str) -> TaskStreamKey {
        TaskStreamKey::new(
            ContextId::new(42, 0),
            TaskId::from_external(ExternalId::new(seed)),
        )
    }

    fn submitted_props() -> A2ATaskStateProps {
        A2ATaskStateProps::new(
            TaskNodeId::new("task:t-broadcaster"),
            TaskStatusKind::Submitted,
            None,
            0,
            anchor(),
        )
    }

    fn anchor() -> ActivityAnchorId {
        ActivityAnchorId::from_counter(1)
    }

    #[tokio::test]
    async fn writer_send_reaches_live_subscriber() {
        let bc = TaskUpdateBroadcaster::default();
        let k = key("t1");
        let mut rx = bc.subscribe(&k);
        let writer = bc.writer(k);

        let sent = writer
            .send(TaskUpdateFrame::StatusTransition {
                state: submitted_props(),
                cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor())
                    .expect("cursor"),
            })
            .expect("send to live receiver");
        assert_eq!(sent, 1, "exactly one receiver got the frame");

        let frame = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("recv within deadline")
            .expect("frame delivered");
        match frame {
            TaskUpdateFrame::StatusTransition { state, .. } => {
                assert_eq!(state.new_status, TaskStatusKind::Submitted);
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_then_writer_share_same_channel() {
        let bc = TaskUpdateBroadcaster::default();
        let k = key("t-shared");
        let mut rx_a = bc.subscribe(&k);
        let mut rx_b = bc.subscribe(&k);
        let writer = bc.writer(k);

        let _ = writer
            .send(TaskUpdateFrame::StatusTransition {
                state: submitted_props(),
                cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor())
                    .expect("cursor"),
            })
            .expect("send to two receivers");

        for rx in [&mut rx_a, &mut rx_b] {
            let frame = tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .expect("recv within deadline")
                .expect("frame delivered");
            assert!(matches!(frame, TaskUpdateFrame::StatusTransition { .. }));
        }
    }

    #[tokio::test]
    async fn retire_task_closes_live_subscribers() {
        let bc = TaskUpdateBroadcaster::default();
        let k = key("t-retire");
        let mut rx = bc.subscribe(&k);
        let writer = bc.writer(k.clone());

        writer.retire_task();

        let res = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
        match res {
            Ok(Err(broadcast::error::RecvError::Closed)) => {}
            other => panic!("expected RecvError::Closed after retire_task, got {other:?}"),
        }

        // Subscribing again creates a brand new channel — the prior one
        // is gone from the registry.
        let _rx2 = bc.subscribe(&k);
    }

    #[tokio::test]
    async fn writer_drop_does_not_remove_channel() {
        let bc = TaskUpdateBroadcaster::default();
        let k = key("t-drop");
        let mut rx = bc.subscribe(&k);
        {
            let writer = bc.writer(k.clone());
            let _ = writer
                .send(TaskUpdateFrame::StatusTransition {
                    state: submitted_props(),
                    cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor())
                        .expect("cursor"),
                })
                .expect("send before writer drop");
        }
        // Drop happened. A second writer must still be able to deliver
        // a frame to the same receiver because the channel is sticky
        // until retire_task.
        let writer2 = bc.writer(k);
        let _ = writer2
            .send(TaskUpdateFrame::StatusTransition {
                state: submitted_props(),
                cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor())
                    .expect("cursor"),
            })
            .expect("send after writer drop");

        // Drain both frames.
        for _ in 0..2 {
            let frame = tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .expect("recv within deadline")
                .expect("frame delivered");
            assert!(matches!(frame, TaskUpdateFrame::StatusTransition { .. }));
        }
    }

    #[tokio::test]
    async fn lagged_subscriber_observes_lagged_error() {
        // Capacity 4 + 8 frames = 4 dropped on the slowest subscriber.
        let bc = TaskUpdateBroadcaster::with_capacity(4);
        let k = key("t-lag");
        let mut rx = bc.subscribe(&k);
        let writer = bc.writer(k);
        for _ in 0..8 {
            let _ = writer.send(TaskUpdateFrame::StatusTransition {
                state: submitted_props(),
                cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor())
                    .expect("cursor"),
            });
        }

        let mut saw_lag = false;
        for _ in 0..8 {
            match rx.try_recv() {
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    saw_lag = true;
                    break;
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
        assert!(
            saw_lag,
            "expected RecvError::Lagged when producer outpaces consumer beyond capacity"
        );
    }
}
