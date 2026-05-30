// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Progress probe for the QuickJS event-loop thread.
//!
//! `quickjs_runtime` runs JavaScript on a dedicated `std::thread`, independent
//! of any tokio worker. A CPU peg confined to that thread (notably an agent
//! whose top-level evaluation runs a tight loop during deploy boot) leaves
//! every tokio worker free, so `RuntimeProgressMeter`'s tokio ticker keeps
//! reporting zero lag while the agent is wedged.
//!
//! [`JsEventLoopProbe`] closes that gap. It posts a no-op task to the JS
//! event loop and awaits an ack on a oneshot channel; while the JS thread is
//! busy, the no-op queues behind the in-flight evaluation and the probe's
//! `last_response_at` does not advance. The post path is non-blocking
//! (`add_task_to_event_loop_void` is unbounded-channel send), so a peg never
//! ties up the tokio task that drives the probe — only the ack `await`
//! resumes when the JS thread drains its queue.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use baml_rt_core::ProgressProbe;
use quickjs_runtime::facades::QuickJsRuntimeFacade;

const PING_INTERVAL: Duration = Duration::from_millis(100);

/// Probes a [`QuickJsRuntimeFacade`]'s event-loop thread to surface CPU pegs
/// that never reach the tokio scheduler.
#[derive(Debug)]
pub struct JsEventLoopProbe {
    started_at: Instant,
    /// Millisecond offset from `started_at` at which the most recent ping
    /// returned. While the JS thread is pegged, the next ping queues behind
    /// the in-flight evaluation and this value stops advancing.
    last_response_at_millis: AtomicU64,
}

impl ProgressProbe for JsEventLoopProbe {
    fn lag_millis(&self) -> u64 {
        let now = self.started_at.elapsed().as_millis() as u64;
        let last = self.last_response_at_millis.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }
}

impl JsEventLoopProbe {
    /// Spawn a tokio task that pings `runtime`'s event loop on `PING_INTERVAL`
    /// and updates the probe's `last_response_at` when each ping completes.
    /// The task exits cleanly when either the probe `Arc` or the runtime
    /// `Arc` is dropped, so callers do not need to manage its lifetime
    /// explicitly — just keep the returned probe alive for as long as the
    /// resource it observes.
    pub fn spawn_for_runtime(runtime: &Arc<QuickJsRuntimeFacade>) -> Arc<Self> {
        let probe = Arc::new(Self {
            started_at: Instant::now(),
            last_response_at_millis: AtomicU64::new(0),
        });
        let weak_probe = Arc::downgrade(&probe);
        let weak_runtime = Arc::downgrade(runtime);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PING_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(probe) = weak_probe.upgrade() else {
                    return;
                };
                let Some(rt) = weak_runtime.upgrade() else {
                    return;
                };

                // Non-blocking: posts a fire-and-forget closure that signals
                // the oneshot when it runs. The closure queues behind any
                // in-flight JS work, so during a peg the ack `await` below
                // is what registers as lag — not the post itself.
                let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<()>();
                rt.add_task_to_event_loop_void(move || {
                    let _ = resp_tx.send(());
                });
                if resp_rx.await.is_err() {
                    // Runtime shut down before the ack landed; exit cleanly.
                    return;
                }

                let elapsed = probe.started_at.elapsed().as_millis() as u64;
                probe
                    .last_response_at_millis
                    .store(elapsed, Ordering::Relaxed);
            }
        });
        probe
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use quickjs_runtime::builder::QuickJsRuntimeBuilder;

    use super::*;

    #[tokio::test]
    async fn idle_runtime_keeps_probe_lag_low() {
        let runtime = Arc::new(QuickJsRuntimeBuilder::new().build());
        let probe = JsEventLoopProbe::spawn_for_runtime(&runtime);

        tokio::time::sleep(Duration::from_millis(300)).await;

        let lag = probe.lag_millis();
        assert!(
            lag < 250,
            "idle JS runtime should not register significant lag (got {lag}ms)"
        );
    }

    #[tokio::test]
    async fn pegged_runtime_drives_probe_lag_above_threshold() {
        let runtime = Arc::new(QuickJsRuntimeBuilder::new().build());
        let probe = JsEventLoopProbe::spawn_for_runtime(&runtime);

        // Let the probe complete at least one round-trip on an idle loop so
        // `last_response_at` is anchored before we peg the thread.
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Peg the JS thread for ~600ms via a fire-and-forget closure scheduled
        // on the event loop. This mirrors how a real CPU-bound JS top-level
        // pegs the thread without going through `runtime.eval` (whose future
        // is `!Send` and therefore awkward to spawn from a tokio test).
        runtime.add_task_to_event_loop_void(|| {
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_millis(600) {
                std::hint::black_box(0);
            }
        });

        tokio::time::sleep(Duration::from_millis(400)).await;
        let lag_during_peg = probe.lag_millis();
        assert!(
            lag_during_peg > 200,
            "pegged JS thread should drive probe lag above 200ms (got {lag_during_peg}ms)"
        );

        // Allow the peg to release plus at least one ping round-trip so the
        // probe's `last_response_at` advances and lag drops back near zero.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let lag_after_peg = probe.lag_millis();
        assert!(
            lag_after_peg < 250,
            "lag should recover after the peg releases (got {lag_after_peg}ms)"
        );
    }
}
