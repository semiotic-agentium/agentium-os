// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Session-mode sandbox pool — Phase 3 of `plans/sandbox_streaming.md` §6.
//!
//! Sits in front of a [`SandboxProvider`] for `invocation_mode=session`
//! tools. Each [`SandboxCacheKey`] maps to a bounded multi-entry pool whose
//! entries cycle through `Idle | Live(session_id) | Draining`.
//!
//! Single-shot tools continue to use [`super::invoker::SandboxCache`]; this
//! pool is intentionally separate so the single-shot fast path is untouched
//! while the session-mode lifecycle gains its own state machine.
//!
//! ## Invariants
//! - One sandbox = at most one live session (§6.2). Concurrent sessions for
//!   the same [`SandboxCacheKey`] each get a distinct entry.
//! - Checkout returning [`PoolError::Exhausted`] is `HostRetriable` per §8.
//! - `Draining` entries are not eligible for checkout. They are destroyed
//!   when their live session exits or, if already idle, immediately.
//!
//! Eviction policies (idle TTL, max session duration, operator quarantine)
//! land in follow-up changes — the entry-state machine is shaped to absorb
//! them without API churn.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use baml_rt_core::{BamlRtError, ClassifiedToolError, ErrorDisposition, Result};
use baml_sandbox_protocol::session::error_code;
use tokio::sync::Notify;
use tracing::{debug, warn};

use super::{
    invoker::{SandboxCacheKey, SandboxSpecBuilder},
    provider::SandboxProvider,
    spec::SandboxHandle,
};
use crate::ToolName;

/// Default upper bound on per-key sandbox count. Per-tool overrides (Phase 4
/// metadata wiring) can replace this via [`SessionPoolConfig::default_pool_max`].
pub const DEFAULT_POOL_MAX: usize = 1;

/// Default time a checkout waits for an `Idle` entry before erroring out.
pub const DEFAULT_POOL_CHECKOUT_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Pool-wide configuration. Per-tool fields will be layered on later via a
/// `(tool_name → ToolPoolConfig)` map.
#[derive(Debug, Clone)]
pub struct SessionPoolConfig {
    pub default_pool_max: usize,
    pub pool_checkout_timeout: Duration,
}

impl Default for SessionPoolConfig {
    fn default() -> Self {
        Self {
            default_pool_max: DEFAULT_POOL_MAX,
            pool_checkout_timeout: DEFAULT_POOL_CHECKOUT_TIMEOUT,
        }
    }
}

/// Errors surfaced by the pool *before* the session is checked out. Failures
/// emitted *during* the session bubble up via [`BamlRtError`] from the
/// provider.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("pool for tool '{tool}' exhausted (cap {cap}); waited {waited:?}")]
    Exhausted {
        tool: ToolName,
        cap: usize,
        waited: Duration,
    },
    #[error("pool checkout failed: {0}")]
    Provider(#[from] BamlRtError),
}

impl PoolError {
    /// Pool-exhaustion errors are [`ErrorDisposition::HostRetriable`] per §8.
    pub fn into_baml_error(self) -> BamlRtError {
        match self {
            PoolError::Exhausted { tool, cap, waited } => {
                BamlRtError::ToolClassified(ClassifiedToolError {
                    code: error_code::POOL_EXHAUSTED.to_string(),
                    disposition: ErrorDisposition::HostRetriable,
                    message: format!(
                        "session pool for '{tool}' is at capacity {cap} (waited {waited:?})"
                    ),
                    hint: Some(
                        "retry after another session finishes or raise pool_max in tool metadata"
                            .to_string(),
                    ),
                    retry_after_ms: Some(50),
                })
            }
            PoolError::Provider(err) => err,
        }
    }
}

/// Live session id stamped on each `Live` entry. Distinct from the
/// adapter-issued session id so the pool can reason about its own state
/// without coupling to wire format.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PooledSessionId(uuid::Uuid);

impl PooledSessionId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Per-entry lifecycle state. See `plans/sandbox_streaming.md` §6.1.
#[derive(Debug, Clone)]
enum EntryState {
    Idle,
    Live {
        session: PooledSessionId,
    },
    /// Slot claimed by a checkout that hasn't finished `provider.create()`
    /// yet. Counts against `count_active()` so a concurrent checkout can't
    /// observe `count < cap` and start a duplicate boot. Promoted to `Live`
    /// on success or removed on failure / cancellation.
    Reserving {
        id: PooledSessionId,
    },
    /// Reserved for cap reduction and operator eviction (§6.3). Wired in a
    /// follow-up alongside the eviction loop.
    #[expect(
        dead_code,
        reason = "Draining state reserved for the upcoming eviction-loop follow-up"
    )]
    Draining,
}

#[derive(Debug)]
struct PoolEntry {
    /// `None` while the entry is `Reserving` (boot still in flight).
    /// `Some` for `Idle` / `Live` / `Draining` — invariant enforced by the
    /// state-transition sites.
    handle: Option<SandboxHandle>,
    state: EntryState,
    last_idle_at: Instant,
}

#[derive(Default)]
struct KeyPool {
    entries: Vec<PoolEntry>,
    /// FIFO of waiters parked when the pool is at cap. Each `Notify` is
    /// released when an entry transitions back to `Idle` or is destroyed
    /// (freeing a slot for cold-create).
    waiters: VecDeque<Arc<Notify>>,
}

impl KeyPool {
    fn idle_index(&self) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| matches!(e.state, EntryState::Idle))
    }

    fn count_active(&self) -> usize {
        self.entries.len()
    }

    fn notify_one_waiter(&mut self) {
        if let Some(waiter) = self.waiters.pop_front() {
            waiter.notify_one();
        }
    }
}

/// Bounded session pool keyed by [`SandboxCacheKey`].
pub struct SessionPool {
    runner_id: String,
    provider: Arc<dyn SandboxProvider>,
    build_spec: SandboxSpecBuilder,
    config: SessionPoolConfig,
    inner: Arc<tokio::sync::Mutex<HashMap<SandboxCacheKey, KeyPool>>>,
}

impl SessionPool {
    pub fn new(
        runner_id: impl Into<String>,
        provider: Arc<dyn SandboxProvider>,
        build_spec: SandboxSpecBuilder,
        config: SessionPoolConfig,
    ) -> Self {
        Self {
            runner_id: runner_id.into(),
            provider,
            build_spec,
            config,
            inner: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    pub fn config(&self) -> &SessionPoolConfig {
        &self.config
    }

    /// Provider used to create / teardown sandboxes. Exposed for the
    /// session invoker which opens persistent channels via
    /// [`SandboxProvider::rpc_channel`].
    pub fn provider(&self) -> &Arc<dyn SandboxProvider> {
        &self.provider
    }

    /// Active entry count across all keys (Idle + Live + Draining).
    pub async fn active_count(&self) -> usize {
        let guard = self.inner.lock().await;
        guard.values().map(|p| p.count_active()).sum()
    }

    /// Reserve a sandbox for one session. Either returns an existing `Idle`
    /// entry, cold-creates one (under cap), or waits up to
    /// [`SessionPoolConfig::pool_checkout_timeout`] before returning
    /// [`PoolError::Exhausted`] (`HostRetriable`).
    pub async fn checkout(self: &Arc<Self>, key: &SandboxCacheKey) -> Result<PooledSandbox> {
        let started = Instant::now();
        let cap = self.config.default_pool_max.max(1);

        loop {
            let waiter = {
                let mut guard = self.inner.lock().await;
                let pool = guard.entry(key.clone()).or_default();

                let mut expired_handles = Vec::new();
                let mut idx = 0;
                while idx < pool.entries.len() {
                    let expired_idle = matches!(pool.entries[idx].state, EntryState::Idle)
                        && pool.entries[idx]
                            .handle
                            .as_ref()
                            .map(|h| h.is_expired())
                            .unwrap_or(true);
                    if expired_idle {
                        let entry = pool.entries.swap_remove(idx);
                        if let Some(handle) = entry.handle {
                            expired_handles.push(handle);
                        }
                    } else {
                        idx += 1;
                    }
                }
                if !expired_handles.is_empty() {
                    pool.notify_one_waiter();
                    drop(guard);
                    for handle in expired_handles {
                        if let Err(err) = self.provider.teardown(&handle).await {
                            warn!(
                                sandbox = %handle.name,
                                error = %err,
                                "session pool: teardown of expired idle sandbox failed"
                            );
                        }
                    }
                    continue;
                }

                if let Some(idx) = pool.idle_index() {
                    let session = PooledSessionId::new();
                    let entry = &mut pool.entries[idx];
                    entry.state = EntryState::Live {
                        session: session.clone(),
                    };
                    let handle = entry
                        .handle
                        .clone()
                        .expect("Idle pool entry must carry a SandboxHandle");
                    debug!(
                        sandbox = %handle.name,
                        tool = %key.tool_name,
                        "session pool: reused idle entry"
                    );
                    return Ok(PooledSandbox {
                        pool: Arc::clone(self),
                        key: key.clone(),
                        handle,
                        session,
                        released: false,
                    });
                }

                if pool.count_active() < cap {
                    // Claim the cap slot under the lock *before* paying boot
                    // cost. Two checkouts that both observe `count < cap`
                    // can't race here: the first push bumps `count` so the
                    // second sees `count == cap` and falls into the waiter
                    // queue. Cancellation / boot failure removes the
                    // reservation via `ReservationGuard::Drop`.
                    let reservation_id = PooledSessionId::new();
                    pool.entries.push(PoolEntry {
                        handle: None,
                        state: EntryState::Reserving {
                            id: reservation_id.clone(),
                        },
                        last_idle_at: Instant::now(),
                    });
                    drop(guard);
                    return self.cold_create(key, reservation_id).await;
                }

                let waiter = Arc::new(Notify::new());
                pool.waiters.push_back(Arc::clone(&waiter));
                waiter
            };

            let elapsed = started.elapsed();
            if elapsed >= self.config.pool_checkout_timeout {
                self.cancel_waiter(key, &waiter).await;
                return Err(PoolError::Exhausted {
                    tool: key.tool_name.clone(),
                    cap,
                    waited: elapsed,
                }
                .into_baml_error());
            }
            let remaining = self.config.pool_checkout_timeout - elapsed;
            match tokio::time::timeout(remaining, waiter.notified()).await {
                Ok(()) => continue,
                Err(_) => {
                    self.cancel_waiter(key, &waiter).await;
                    return Err(PoolError::Exhausted {
                        tool: key.tool_name.clone(),
                        cap,
                        waited: started.elapsed(),
                    }
                    .into_baml_error());
                }
            }
        }
    }

    /// Run the boot for a reservation slot already pushed by
    /// [`Self::checkout`]. On any error path the [`ReservationGuard`] removes
    /// the placeholder entry and notifies a waiter; on success we promote
    /// `Reserving → Live` under the lock and disarm the guard.
    async fn cold_create(
        self: &Arc<Self>,
        key: &SandboxCacheKey,
        reservation_id: PooledSessionId,
    ) -> Result<PooledSandbox> {
        let mut guard =
            ReservationGuard::new(Arc::clone(self), key.clone(), reservation_id.clone());

        let spec = (self.build_spec)(key)?;
        let handle = self.provider.create(spec).await?;
        guard.set_handle(handle.clone());

        let session = PooledSessionId::new();

        {
            let mut pool_guard = self.inner.lock().await;
            let Some(pool) = pool_guard.get_mut(key) else {
                // Pool entry vanished — only happens if the key was drained.
                // Guard's drop will tear down the booted handle.
                return Err(BamlRtError::ToolExecution(format!(
                    "session pool: key '{}' missing while completing reservation",
                    key.tool_name
                )));
            };
            let Some(idx) = pool.entries.iter().position(
                |e| matches!(&e.state, EntryState::Reserving { id } if id == &reservation_id),
            ) else {
                // Reservation was removed (e.g. by an external cleanup).
                // Guard's drop tears down the handle.
                return Err(BamlRtError::ToolExecution(format!(
                    "session pool: reservation lost for tool '{}'",
                    key.tool_name
                )));
            };
            let entry = &mut pool.entries[idx];
            entry.state = EntryState::Live {
                session: session.clone(),
            };
            entry.handle = Some(handle.clone());
            entry.last_idle_at = Instant::now();
        }

        guard.commit();

        Ok(PooledSandbox {
            pool: Arc::clone(self),
            key: key.clone(),
            handle,
            session,
            released: false,
        })
    }

    /// Cleanup path for a reservation that was never promoted to `Live`
    /// (boot failure, future cancellation, lost pool entry). Removes the
    /// placeholder entry, notifies a waiter so a queued checkout can take
    /// the freed slot, and tears down any handle the booter managed to
    /// produce.
    async fn drop_reservation(
        &self,
        key: &SandboxCacheKey,
        reservation_id: &PooledSessionId,
        handle: Option<SandboxHandle>,
    ) {
        {
            let mut pool_guard = self.inner.lock().await;
            if let Some(pool) = pool_guard.get_mut(key) {
                pool.entries.retain(
                    |e| !matches!(&e.state, EntryState::Reserving { id } if id == reservation_id),
                );
                pool.notify_one_waiter();
            }
        }
        if let Some(handle) = handle
            && let Err(err) = self.provider.teardown(&handle).await
        {
            warn!(
                sandbox = %handle.name,
                error = %err,
                "session pool: teardown of cancelled reservation failed"
            );
        }
    }

    async fn cancel_waiter(&self, key: &SandboxCacheKey, waiter: &Arc<Notify>) {
        let mut guard = self.inner.lock().await;
        if let Some(pool) = guard.get_mut(key) {
            pool.waiters.retain(|w| !Arc::ptr_eq(w, waiter));
        }
    }

    async fn release(
        &self,
        key: &SandboxCacheKey,
        session: &PooledSessionId,
        disposition: Release,
    ) {
        // Find the entry, then either teardown or mark idle. Teardown happens
        // after the lock is released so the provider call doesn't block other
        // checkouts.
        let teardown_handle = {
            let mut guard = self.inner.lock().await;
            let Some(pool) = guard.get_mut(key) else {
                return;
            };
            let Some(idx) = pool.entries.iter().position(
                |e| matches!(&e.state, EntryState::Live { session: live } if live == session),
            ) else {
                return;
            };

            match disposition {
                Release::ReturnIdle => {
                    let entry = &mut pool.entries[idx];
                    entry.state = EntryState::Idle;
                    entry.last_idle_at = Instant::now();
                    if let Some(handle) = entry.handle.as_mut() {
                        handle.touch();
                    }
                    pool.notify_one_waiter();
                    None
                }
                Release::Destroy => {
                    let entry = pool.entries.swap_remove(idx);
                    pool.notify_one_waiter();
                    Some(
                        entry
                            .handle
                            .expect("Live pool entry must carry a SandboxHandle"),
                    )
                }
            }
        };

        if let Some(handle) = teardown_handle
            && let Err(err) = self.provider.teardown(&handle).await
        {
            warn!(
                sandbox = %handle.name,
                error = %err,
                "session pool: teardown failed; sandbox abandoned"
            );
        }
    }
}

/// RAII helper for a reservation slot held by [`SessionPool::cold_create`].
/// Guarantees that any path which leaves the boot future without producing a
/// [`PooledSandbox`] (boot error, lock-time invariant break, future
/// cancellation) removes the placeholder entry, notifies a waiter, and tears
/// down the booted handle if one was already produced.
struct ReservationGuard {
    pool: Arc<SessionPool>,
    key: SandboxCacheKey,
    id: PooledSessionId,
    /// Populated once `provider.create()` returns successfully. Cleared by
    /// [`Self::commit`] so the `Drop` no-op path doesn't double-teardown.
    handle: Option<SandboxHandle>,
    committed: bool,
}

impl ReservationGuard {
    fn new(pool: Arc<SessionPool>, key: SandboxCacheKey, id: PooledSessionId) -> Self {
        Self {
            pool,
            key,
            id,
            handle: None,
            committed: false,
        }
    }

    fn set_handle(&mut self, handle: SandboxHandle) {
        self.handle = Some(handle);
    }

    /// Mark the reservation as successfully promoted to `Live`. Must be
    /// called only after the lock-protected state transition has been
    /// observed.
    fn commit(mut self) {
        self.committed = true;
        self.handle = None;
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let pool = Arc::clone(&self.pool);
        let key = self.key.clone();
        let id = self.id.clone();
        let handle = self.handle.take();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                pool.drop_reservation(&key, &id, handle).await;
            });
        } else {
            warn!(
                tool = %self.key.tool_name,
                "ReservationGuard dropped outside tokio runtime; \
                 reservation slot leaked until process exit"
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Release {
    /// Sandbox is clean (reset succeeded); return to the `Idle` state.
    ReturnIdle,
    /// Sandbox must be torn down (abort, reset failure, drained pool, etc.).
    Destroy,
}

/// Borrow handle returned by [`SessionPool::checkout`]. Owns no resources
/// itself — finishing or aborting the session is the consumer's
/// responsibility via [`PooledSandbox::release_finish_idle`] /
/// [`PooledSandbox::release_destroy`]. Dropping without releasing destroys
/// the sandbox so the pool doesn't leak entries on a bug.
pub struct PooledSandbox {
    pool: Arc<SessionPool>,
    key: SandboxCacheKey,
    handle: SandboxHandle,
    session: PooledSessionId,
    /// Set by `release_*` so the [`Drop`] guard becomes a no-op and the
    /// release path is observed only once.
    released: bool,
}

impl PooledSandbox {
    pub fn handle(&self) -> &SandboxHandle {
        &self.handle
    }

    pub fn key(&self) -> &SandboxCacheKey {
        &self.key
    }

    /// Return the sandbox to the pool as `Idle`. Callers MUST only invoke
    /// this after a successful `tool/session_finish` *and* a successful
    /// `tool/session_reset` (when reuse is configured). For non-reuse tools,
    /// use [`Self::release_destroy`].
    pub async fn release_finish_idle(mut self) {
        self.released = true;
        let pool = Arc::clone(&self.pool);
        let key = self.key.clone();
        let session = self.session.clone();
        pool.release(&key, &session, Release::ReturnIdle).await;
    }

    /// Tear down the sandbox after a session-ending event (abort, reset
    /// failure, fatal channel error, non-reuse finish, …).
    pub async fn release_destroy(mut self) {
        self.released = true;
        let pool = Arc::clone(&self.pool);
        let key = self.key.clone();
        let session = self.session.clone();
        pool.release(&key, &session, Release::Destroy).await;
    }
}

impl Drop for PooledSandbox {
    fn drop(&mut self) {
        debug_assert!(
            self.released,
            "PooledSandbox dropped without release_finish_idle/release_destroy; \
             tests should always release explicitly so missed paths surface here"
        );
        if self.released {
            return;
        }
        let pool = Arc::clone(&self.pool);
        let key = self.key.clone();
        let session = self.session.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                pool.release(&key, &session, Release::Destroy).await;
            });
        } else {
            warn!(
                sandbox = %self.handle.name,
                "PooledSandbox dropped outside tokio runtime; entry leaked until process exit"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use baml_rt_core::{
        Result,
        ids::{AgentId, ContextId, UuidId},
    };
    use futures_util::stream::{self, BoxStream};

    use super::*;
    use crate::external_tools::sandbox::{
        channel::TsrpcChannel,
        provider::SandboxProvider,
        spec::{SandboxEvent, SandboxSpec},
    };

    #[derive(Default)]
    struct CountingProvider {
        creates: std::sync::atomic::AtomicUsize,
        teardowns: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SandboxProvider for CountingProvider {
        async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle> {
            self.creates
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(SandboxHandle::new(spec.name, Duration::from_secs(60)))
        }

        async fn rpc_channel(&self, _handle: &SandboxHandle) -> Result<TsrpcChannel> {
            unreachable!("pool tests don't open channels")
        }

        async fn teardown(&self, _handle: &SandboxHandle) -> Result<()> {
            self.teardowns
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn events(&self, _handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent> {
            Box::pin(stream::empty())
        }

        async fn list_owned(&self, _runner_id: &str) -> Result<Vec<SandboxHandle>> {
            Ok(Vec::new())
        }

        async fn reattach(&self, _name: &str) -> Result<SandboxHandle> {
            Err(BamlRtError::InvalidArgument(
                "reattach unsupported in pool tests".into(),
            ))
        }
    }

    fn fixture(provider: Arc<CountingProvider>, cap: usize) -> Arc<SessionPool> {
        let build_spec: SandboxSpecBuilder = Arc::new(|key: &SandboxCacheKey| {
            Ok(SandboxSpec::for_test(
                format!("test-{}", key.tool_name),
                "scratch:latest",
            ))
        });
        Arc::new(SessionPool::new(
            "runner-test",
            provider,
            build_spec,
            SessionPoolConfig {
                default_pool_max: cap,
                pool_checkout_timeout: Duration::from_millis(50),
            },
        ))
    }

    fn sample_key(tool: &str) -> SandboxCacheKey {
        SandboxCacheKey {
            agent_id: AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4())),
            context_id: ContextId::new(1, 1),
            tool_name: ToolName::parse(tool).unwrap(),
        }
    }

    /// Cold-create on first checkout, reuse the same sandbox after release,
    /// and tear down on destroy.
    #[tokio::test]
    async fn cold_create_then_reuse_then_destroy() {
        let provider = Arc::new(CountingProvider::default());
        let pool = fixture(provider.clone(), 1);
        let key = sample_key("support/session_reuse");

        let s1 = pool.checkout(&key).await.expect("first checkout");
        let name1 = s1.handle().name.clone();
        s1.release_finish_idle().await;

        let s2 = pool.checkout(&key).await.expect("second checkout reuses");
        assert_eq!(s2.handle().name, name1, "expected idle reuse");
        s2.release_destroy().await;

        assert_eq!(
            provider.creates.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            provider.teardowns.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(pool.active_count().await, 0);
    }

    /// At-cap with a live session, second checkout times out with the
    /// pool-exhausted classification (HostRetriable, code=pool_exhausted).
    #[tokio::test]
    async fn checkout_times_out_when_pool_at_cap() {
        let provider = Arc::new(CountingProvider::default());
        let pool = fixture(provider, 1);
        let key = sample_key("support/session_cap");

        let live = pool.checkout(&key).await.expect("live checkout");
        let err = match pool.checkout(&key).await {
            Ok(_) => panic!("second checkout must time out"),
            Err(err) => err,
        };

        match err {
            BamlRtError::ToolClassified(c) => {
                assert_eq!(c.code, error_code::POOL_EXHAUSTED);
                assert_eq!(c.disposition, ErrorDisposition::HostRetriable);
            }
            other => panic!("expected ToolClassified, got {other:?}"),
        }
        live.release_destroy().await;
    }
}
