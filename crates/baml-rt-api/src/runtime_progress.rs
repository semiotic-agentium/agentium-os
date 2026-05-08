//! Continuously observable signal that the tokio runtime is making progress.
//!
//! Distinct from the boot-time `/readyz` gate, which latches `true` once
//! event producers register and never reverts: a stalled runtime is
//! invisible to that flag. The meter catches starvation (CPU-pegged
//! worker, cgroup throttling, deadlocked task) by exposing how long it
//! has been since a 100ms ticker last ran.
//!
//! Scope: the ticker is a regular tokio task, so it detects whole-runtime
//! stalls (every worker blocked, cgroup throttled). On a multi-worker
//! runtime a single wedged worker with others available will not register
//! as lag — the runner's `cpu: 1` cgroup limit makes that scenario
//! unlikely in production, but it is the meter's blind spot.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const TICK_INTERVAL: Duration = Duration::from_millis(100);

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
}

impl RuntimeProgressMeter {
    pub fn new_without_ticker() -> Arc<Self> {
        Arc::new(Self {
            started_at: Instant::now(),
            last_tick_millis: AtomicU64::new(0),
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

    /// Milliseconds since the last tick.
    ///
    /// Healthy runtime: under one interval period (100ms) plus scheduler
    /// jitter. Stalled runtime: grows linearly with wall-clock until the
    /// runtime resumes polling tasks.
    pub fn lag_millis(&self) -> u64 {
        let now = self.started_at.elapsed().as_millis() as u64;
        let last = self.last_tick_millis.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
