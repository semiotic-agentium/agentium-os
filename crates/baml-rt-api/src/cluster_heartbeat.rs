// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Operator-visible health for the cluster heartbeat task.
//!
//! Surfaced by `GET /diagnose` as `cluster_heartbeat_status`,
//! `cluster_heartbeat_lag_ms`, and (after the first failure)
//! `cluster_heartbeat_last_error_kind`.
//!
//! `status()` distinguishes three states:
//! * [`HeartbeatStatus::Starting`] — no heartbeat attempt has completed yet
//!   (fresh runner, ~10s window after pod start).
//! * [`HeartbeatStatus::Ok`] — the most recent attempt succeeded *and* its
//!   lag is within [`ClusterHeartbeatHealth::STALE_LAG_MULTIPLIER`] ×
//!   `interval`.
//! * [`HeartbeatStatus::Degraded`] — the last attempt errored, or the last
//!   success is now stale.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use baml_rt_core::HeartbeatErrorKind;

/// Operator-visible heartbeat status surfaced on `GET /diagnose`.
///
/// `Starting` separates "pod hasn't completed its first tick" from
/// `Degraded`, so dashboards can suppress alerts during the boot window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HeartbeatStatus {
    Starting,
    Ok,
    Degraded,
}

#[derive(Debug)]
pub struct ClusterHeartbeatHealth {
    started_at: Instant,
    interval: Duration,
    /// Set to `true` once any heartbeat attempt — success or failure — has
    /// completed. Synchronization gate for `status()`: a reader that sees
    /// this `true` is guaranteed to see the matching `last_attempt_ok` and
    /// `last_error_kind` writes.
    has_ever_attempted: AtomicBool,
    /// Set to `true` once at least one heartbeat has succeeded.
    /// Synchronization gate for `lag_millis()`: a reader that sees this
    /// `true` is guaranteed to see the matching `last_ok_millis` write.
    has_ever_succeeded: AtomicBool,
    last_ok_millis: AtomicU64,
    last_attempt_ok: AtomicBool,
    /// Encoded [`HeartbeatErrorKind`] of the most recently observed
    /// failure. Only meaningful when `has_ever_attempted` is true and
    /// `last_attempt_ok` is false.
    last_error_kind: AtomicU8,
}

impl ClusterHeartbeatHealth {
    /// `status()` reports `Ok` only when `lag <= STALE_LAG_MULTIPLIER * interval`,
    /// so a single missed beat does not flip the status to `Degraded`.
    pub const STALE_LAG_MULTIPLIER: u64 = 2;

    #[must_use]
    pub fn new(interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            started_at: Instant::now(),
            interval,
            has_ever_attempted: AtomicBool::new(false),
            has_ever_succeeded: AtomicBool::new(false),
            last_ok_millis: AtomicU64::new(0),
            last_attempt_ok: AtomicBool::new(false),
            last_error_kind: AtomicU8::new(encode_kind(HeartbeatErrorKind::Other)),
        })
    }

    fn elapsed_millis(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    pub fn record_ok(&self) {
        let now = self.elapsed_millis();
        self.last_ok_millis.store(now, Ordering::Relaxed);
        self.last_attempt_ok.store(true, Ordering::Relaxed);
        self.has_ever_succeeded.store(true, Ordering::Release);
        self.has_ever_attempted.store(true, Ordering::Release);
    }

    pub fn record_error(&self, kind: HeartbeatErrorKind) {
        self.last_error_kind
            .store(encode_kind(kind), Ordering::Relaxed);
        self.last_attempt_ok.store(false, Ordering::Relaxed);
        self.has_ever_attempted.store(true, Ordering::Release);
    }

    /// Milliseconds since the last successful heartbeat. `None` when no
    /// heartbeat has ever succeeded.
    #[must_use]
    pub fn lag_millis(&self) -> Option<u64> {
        // `has_ever_succeeded` is the synchronization gate: its `Acquire`
        // load fences the corresponding `Release` write in `record_ok`,
        // so the subsequent `Relaxed` load of `last_ok_millis` is
        // guaranteed to see the matching value.
        if !self.has_ever_succeeded.load(Ordering::Acquire) {
            return None;
        }
        let last = self.last_ok_millis.load(Ordering::Relaxed);
        Some(self.elapsed_millis().saturating_sub(last))
    }

    /// Most recent error class. `None` when no failure has ever been
    /// recorded (boot or all-success path).
    #[must_use]
    pub fn last_error_kind(&self) -> Option<HeartbeatErrorKind> {
        // Same gating discipline as `lag_millis`: `has_ever_attempted`
        // Acquire fences the Release write that paired with the
        // Relaxed kind store. Returning `None` when the last attempt
        // succeeded keeps the field sticky-on-fault: it appears once
        // an error has been seen and stays present even if subsequent
        // ticks succeed (so operators can still see "what broke").
        if !self.has_ever_attempted.load(Ordering::Acquire) {
            return None;
        }
        if self.last_attempt_ok.load(Ordering::Relaxed) {
            return None;
        }
        Some(decode_kind(self.last_error_kind.load(Ordering::Relaxed)))
    }

    /// Whether this heartbeat's current state should leave `/readyz` passing.
    ///
    /// Returns `false` only when the heartbeat has previously succeeded **and**
    /// is now [`HeartbeatStatus::Degraded`] — i.e. "this runner was healthy
    /// and is now not." Once a pod has confirmed it CAN talk to the cluster
    /// registry, subsequent `Degraded` blocks new traffic so kubelet stops
    /// routing sessions to a pod whose placement updates will not durably land.
    ///
    /// Fresh pods are intentionally allowed through: a never-succeeded pod
    /// in `Starting` or `Degraded` keeps `/readyz` at `200`. SurrealDB-slow-
    /// during-boot must not pin a fresh pod at `503` forever, and an
    /// initial heartbeat failure is part of normal boot-window behaviour.
    #[must_use]
    pub fn is_within_readyz_threshold(&self) -> bool {
        if !self.has_ever_succeeded.load(Ordering::Acquire) {
            return true;
        }
        self.status() != HeartbeatStatus::Degraded
    }

    #[must_use]
    pub fn status(&self) -> HeartbeatStatus {
        if !self.has_ever_attempted.load(Ordering::Acquire) {
            return HeartbeatStatus::Starting;
        }
        if !self.last_attempt_ok.load(Ordering::Relaxed) {
            return HeartbeatStatus::Degraded;
        }
        let stale_threshold = Self::STALE_LAG_MULTIPLIER * self.interval.as_millis() as u64;
        match self.lag_millis() {
            Some(lag) if lag <= stale_threshold => HeartbeatStatus::Ok,
            _ => HeartbeatStatus::Degraded,
        }
    }
}

fn encode_kind(kind: HeartbeatErrorKind) -> u8 {
    match kind {
        HeartbeatErrorKind::Connection => 0,
        HeartbeatErrorKind::Query => 1,
        HeartbeatErrorKind::NotAllowed => 2,
        HeartbeatErrorKind::Other => 3,
    }
}

fn decode_kind(raw: u8) -> HeartbeatErrorKind {
    match raw {
        0 => HeartbeatErrorKind::Connection,
        1 => HeartbeatErrorKind::Query,
        2 => HeartbeatErrorKind::NotAllowed,
        _ => HeartbeatErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_meter_is_starting_with_none_lag_and_no_error_kind() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        assert_eq!(health.status(), HeartbeatStatus::Starting);
        assert_eq!(health.lag_millis(), None);
        assert_eq!(health.last_error_kind(), None);
    }

    #[test]
    fn recording_ok_makes_status_ok_and_lag_some() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        health.record_ok();
        assert_eq!(health.status(), HeartbeatStatus::Ok);
        assert_eq!(
            health.last_error_kind(),
            None,
            "no error has been recorded; last_error_kind stays None"
        );
        let lag = health
            .lag_millis()
            .expect("lag should be Some after record_ok");
        assert!(
            lag < 100,
            "lag should be near zero immediately after record_ok (got {lag})"
        );
    }

    #[test]
    fn recording_error_after_ok_marks_degraded_and_surfaces_kind() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        health.record_ok();
        health.record_error(HeartbeatErrorKind::Connection);
        assert_eq!(
            health.status(),
            HeartbeatStatus::Degraded,
            "a failed attempt after a successful one must downgrade status"
        );
        assert!(
            health.lag_millis().is_some(),
            "lag should still report time-since-last-ok even after a failed attempt"
        );
        assert_eq!(
            health.last_error_kind(),
            Some(HeartbeatErrorKind::Connection),
            "kind from the most recent failure must be retrievable"
        );
    }

    #[test]
    fn recovery_clears_last_error_kind() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        health.record_error(HeartbeatErrorKind::Query);
        assert_eq!(health.last_error_kind(), Some(HeartbeatErrorKind::Query));
        health.record_ok();
        assert_eq!(
            health.last_error_kind(),
            None,
            "after recovery the field hides until the next failure"
        );
    }

    #[test]
    fn readyz_gate_passes_fresh_pod_in_starting() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        assert_eq!(health.status(), HeartbeatStatus::Starting);
        assert!(
            health.is_within_readyz_threshold(),
            "fresh pod (no heartbeat attempt yet) must not 503 /readyz"
        );
    }

    #[test]
    fn readyz_gate_passes_never_succeeded_pod_in_degraded() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        health.record_error(HeartbeatErrorKind::Connection);
        assert_eq!(health.status(), HeartbeatStatus::Degraded);
        assert!(
            health.is_within_readyz_threshold(),
            "boot-window heartbeat failure (no prior success) must not 503 /readyz — \
             SurrealDB-slow-during-boot can't pin fresh pods forever"
        );
    }

    #[test]
    fn readyz_gate_blocks_degraded_after_prior_success() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        health.record_ok();
        health.record_error(HeartbeatErrorKind::Connection);
        assert_eq!(health.status(), HeartbeatStatus::Degraded);
        assert!(
            !health.is_within_readyz_threshold(),
            "a pod that was healthy and is now degraded must 503 /readyz so kubelet \
             stops routing to it"
        );
    }

    #[test]
    fn readyz_gate_passes_after_recovery() {
        let health = ClusterHeartbeatHealth::new(Duration::from_secs(5));
        health.record_ok();
        health.record_error(HeartbeatErrorKind::Connection);
        assert!(
            !health.is_within_readyz_threshold(),
            "degraded after success blocks"
        );
        health.record_ok();
        assert!(
            health.is_within_readyz_threshold(),
            "next successful heartbeat clears the gate so the pod takes traffic again"
        );
    }

    #[test]
    fn stale_ok_reports_degraded() {
        let health = ClusterHeartbeatHealth::new(Duration::from_millis(50));
        health.record_ok();
        // STALE_LAG_MULTIPLIER * interval = 100ms; sleep past it.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            health.status(),
            HeartbeatStatus::Degraded,
            "lag past STALE_LAG_MULTIPLIER × interval must report degraded"
        );
    }
}
