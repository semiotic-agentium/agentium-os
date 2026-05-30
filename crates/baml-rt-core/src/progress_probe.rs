// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Pluggable lag signals for the runtime-progress meter.
//!
//! The runtime-progress meter on its own measures only tokio-task progress, so
//! a CPU peg confined to a thread the tokio scheduler never visits (notably the
//! QuickJS event-loop thread during deploy boot) leaves the meter showing zero
//! lag. A [`ProgressProbe`] crosses that boundary: each implementation reports
//! how stale its underlying signal is, and the meter aggregates the worst lag
//! across itself and every registered probe.

use std::{
    fmt::Debug,
    sync::{Arc, Weak},
};

/// Reports how long it has been since the probed resource last made progress.
///
/// Implementations must keep `lag_millis` cheap — typically a single
/// wall-clock-derived atomic load. Probe reads happen on the `/diagnose`
/// hot path and inside `RuntimeProgressMeter::lag_millis`, so blocking or
/// I/O work in this method would stall every concurrent diagnose request.
pub trait ProgressProbe: Debug + Send + Sync {
    /// Milliseconds since the probe last observed progress.
    fn lag_millis(&self) -> u64;
}

/// A target that accepts probe registrations. Decouples [`ProgressProbe`]
/// producers from the concrete meter type so foundation crates (e.g.
/// `baml-rt-quickjs`) can register probes without depending on the HTTP API
/// crate that hosts the meter.
pub trait ProgressProbeRegistry: Send + Sync {
    /// Attach `probe` to the registry. The registry should hold the weak
    /// reference and drop it when the producer's last strong `Arc` is gone.
    fn register_probe(&self, probe: Weak<dyn ProgressProbe>);
}

/// Convenience for callers that hold an `Arc<P>` where `P: ProgressProbe`:
/// coerces to `Arc<dyn ProgressProbe>`, downgrades, and registers.
pub fn register_progress_probe<P>(registry: &dyn ProgressProbeRegistry, probe: &Arc<P>)
where
    P: ProgressProbe + 'static,
{
    let dyn_probe: Arc<dyn ProgressProbe> = probe.clone();
    registry.register_probe(Arc::downgrade(&dyn_probe));
}
