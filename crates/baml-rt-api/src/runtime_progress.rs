// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Continuously observable signal that the tokio runtime is making progress.
//!
//! Distinct from the boot-time `/readyz` gate, which latches `true` once
//! event producers register and never reverts: a stalled runtime is
//! invisible to that flag. The meter catches starvation (CPU-pegged
//! worker, cgroup throttling, deadlocked task) by exposing how long it
//! has been since a 100ms ticker last ran.
//!
//! Scope: the ticker is a regular tokio task, so on its own it only sees
//! whole-runtime stalls (every worker blocked, cgroup throttled). A single
//! wedged worker, or a CPU peg confined to a non-tokio thread (e.g. the
//! QuickJS event loop during deploy boot) does not register through the
//! ticker alone. To cross those boundaries, callers can attach extra signals
//! via [`RuntimeProgressMeter::register_probe`]; [`lag_millis`](Self::lag_millis)
//! aggregates the worst lag across the ticker and every live probe so the
//! meter reflects a CPU peg wherever it lives.

use std::{
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use baml_rt_core::{ProgressProbe, ProgressProbeRegistry};

const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Threshold above which `GET /readyz` flips to `503 Service Unavailable`.
///
/// Picked at ten ticker periods so normal scheduler jitter (50–250ms under
/// CI load, per the no-load integration test) never trips the gate, while a
/// multi-second runtime stall — the failure mode that motivated issue #339
/// — is caught well before kubelet's `failureThreshold × periodSeconds`
/// window (default 6 × 10s) removes the pod from Service endpoints.
pub const READYZ_LAG_THRESHOLD_MS: u64 = 1_000;

/// Continuously-bumped tokio-runtime progress counter.
///
/// In production, use [`RuntimeProgressMeter::spawn_in_current_runtime`]:
/// the returned `Arc` owns a background ticker that exits when the last
/// strong reference is dropped. Tests can use
/// [`RuntimeProgressMeter::new_without_ticker`] to drive the timestamp
/// deterministically via [`tick`](Self::tick).
#[derive(Debug)]
pub struct RuntimeProgressMeter {
    started_at: Instant,
    last_tick_millis: AtomicU64,
    probes: RwLock<Vec<Weak<dyn ProgressProbe>>>,
}

impl RuntimeProgressMeter {
    pub fn new_without_ticker() -> Arc<Self> {
        Arc::new(Self {
            started_at: Instant::now(),
            last_tick_millis: AtomicU64::new(0),
            probes: RwLock::new(Vec::new()),
        })
    }

    /// # Panics
    /// Panics if called outside a tokio runtime context.
    pub fn spawn_in_current_runtime() -> Arc<Self> {
        Self::spawn_with_handle().0
    }

    pub(crate) fn spawn_with_handle() -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let meter = Self::new_without_ticker();
        let weak = Arc::downgrade(&meter);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TICK_INTERVAL);
            // Skip keeps the cadence anchored to `started_at`: after a stall,
            // one tick fires immediately and the next is scheduled on the
            // original timeline, not shifted forward by the stall duration.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let Some(strong) = weak.upgrade() else {
                    break;
                };
                strong.tick();
            }
        });
        (meter, handle)
    }

    pub fn tick(&self) {
        let elapsed = self.started_at.elapsed().as_millis() as u64;
        self.last_tick_millis.store(elapsed, Ordering::Relaxed);
    }

    /// Attach an external lag signal whose value contributes to
    /// [`lag_millis`](Self::lag_millis). Probes are held weakly: when the
    /// caller drops its last `Arc`, the meter skips the dead reference on the
    /// next read and prunes it on the next call to this method.
    pub fn register_probe(&self, probe: Weak<dyn ProgressProbe>) {
        let mut guard = self.probes.write().expect("probes lock poisoned");
        guard.retain(|p| p.strong_count() > 0);
        guard.push(probe);
    }

    /// Worst-case milliseconds since the last observed progress signal.
    ///
    /// Returns the maximum across the tokio ticker and every registered
    /// probe whose strong reference is still alive. Healthy runtime: under
    /// one interval period (100ms) plus scheduler jitter. Stalled runtime
    /// (anywhere — tokio worker, QuickJS event loop, or any other registered
    /// probe): grows linearly with wall-clock until the underlying resource
    /// resumes making progress.
    pub fn lag_millis(&self) -> u64 {
        let now = self.started_at.elapsed().as_millis() as u64;
        let last = self.last_tick_millis.load(Ordering::Relaxed);
        let mut worst = now.saturating_sub(last);

        let probes = self.probes.read().expect("probes lock poisoned");
        for weak in probes.iter() {
            if let Some(probe) = weak.upgrade() {
                let probe_lag = probe.lag_millis();
                if probe_lag > worst {
                    worst = probe_lag;
                }
            }
        }
        worst
    }

    /// Predicate consumed by `GET /readyz`: the meter is healthy iff its
    /// aggregated lag is below [`READYZ_LAG_THRESHOLD_MS`].
    pub fn is_within_readyz_threshold(&self) -> bool {
        self.lag_millis() < READYZ_LAG_THRESHOLD_MS
    }
}

impl ProgressProbeRegistry for RuntimeProgressMeter {
    fn register_probe(&self, probe: Weak<dyn ProgressProbe>) {
        RuntimeProgressMeter::register_probe(self, probe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedLagProbe(u64);

    impl ProgressProbe for FixedLagProbe {
        fn lag_millis(&self) -> u64 {
            self.0
        }
    }

    #[test]
    fn lag_grows_without_ticker() {
        let meter = RuntimeProgressMeter::new_without_ticker();
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            meter.lag_millis() >= 40,
            "lag should reflect elapsed wall-clock when ticker is absent (got {})",
            meter.lag_millis()
        );
    }

    #[test]
    fn manual_tick_resets_lag() {
        let meter = RuntimeProgressMeter::new_without_ticker();
        std::thread::sleep(Duration::from_millis(50));
        meter.tick();
        assert!(
            meter.lag_millis() < 20,
            "lag should be near zero immediately after tick (got {})",
            meter.lag_millis()
        );
    }

    #[test]
    fn registered_probe_dominates_when_higher() {
        let meter = RuntimeProgressMeter::new_without_ticker();
        meter.tick();
        let probe: Arc<dyn ProgressProbe> = Arc::new(FixedLagProbe(5_000));
        meter.register_probe(Arc::downgrade(&probe));
        let lag = meter.lag_millis();
        assert!(
            lag >= 5_000,
            "probe lag should propagate through the meter (got {lag})"
        );
    }

    #[test]
    fn dropped_probes_do_not_inflate_lag() {
        let meter = RuntimeProgressMeter::new_without_ticker();
        meter.tick();
        {
            let probe: Arc<dyn ProgressProbe> = Arc::new(FixedLagProbe(9_999));
            meter.register_probe(Arc::downgrade(&probe));
        }
        // Probe is dropped; the next read should not see the stale value.
        let lag = meter.lag_millis();
        assert!(
            lag < 50,
            "dropped probe must not contribute to lag (got {lag})"
        );
    }

    #[tokio::test]
    async fn spawn_in_runtime_advances_lag_under_no_load() {
        let meter = RuntimeProgressMeter::spawn_in_current_runtime();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let lag = meter.lag_millis();
        // Looser than the strictly-expected ~50ms: CI workers can slip
        // ticker scheduling by 50–100ms under load.
        assert!(
            lag < 250,
            "under no load, lag should stay near one interval period (got {lag})"
        );
    }

    #[tokio::test]
    async fn ticker_exits_when_meter_dropped() {
        let (meter, handle) = RuntimeProgressMeter::spawn_with_handle();
        drop(meter);
        let result = tokio::time::timeout(Duration::from_millis(300), handle).await;
        let join = result.expect("ticker did not exit within 300ms after meter was dropped");
        assert!(
            join.is_ok(),
            "ticker task panicked instead of exiting cleanly"
        );
    }
}
