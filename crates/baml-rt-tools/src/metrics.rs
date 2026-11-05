// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry metrics instrumentation for tool operations.
//!
//! This module provides orthogonal metrics recording helpers following the pattern
//! from the OpenTelemetry metrics instrumentation guide. All metrics use static
//! names with dynamic data in structured attributes. Instruments are cached
//! using OnceLock for zero-allocation hot paths.

use std::{sync::OnceLock, time::Duration};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram, UpDownCounter},
};

// Static caches - initialized once, reused forever
static TOOL_REGISTRATION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_SESSION_OPEN_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_SESSION_OPERATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

// Getter functions - initialize on first call, return cached reference after
fn tool_registration_counter() -> &'static Counter<u64> {
    TOOL_REGISTRATION_COUNTER.get_or_init(|| {
        global::meter("baml_rt_tools")
            .u64_counter("baml_rt_tools.tool.registration.total")
            .init()
    })
}

fn tool_session_open_counter() -> &'static Counter<u64> {
    TOOL_SESSION_OPEN_COUNTER.get_or_init(|| {
        global::meter("baml_rt_tools")
            .u64_counter("baml_rt_tools.tool.session.open.total")
            .init()
    })
}

fn tool_session_operation_histogram() -> &'static Histogram<f64> {
    TOOL_SESSION_OPERATION_HISTOGRAM.get_or_init(|| {
        global::meter("baml_rt_tools")
            .f64_histogram("baml_rt_tools.tool.session.operation.duration_ms")
            .init()
    })
}

/// Record tool registration event.
pub(crate) fn record_tool_registration(tool_name: &str) {
    tool_registration_counter().add(1, &[KeyValue::new("tool", tool_name.to_string())]);
}

/// Record tool session open event.
pub(crate) fn record_session_open(tool_name: &str) {
    tool_session_open_counter().add(1, &[KeyValue::new("tool", tool_name.to_string())]);
}

/// Record a tool session FSM operation (`open`, `send`, `read`, `finish`, `abort`)
/// with the owning tool name, outcome, and duration.
pub(crate) fn record_session_operation(
    operation: &str,
    tool_name: &str,
    result: &str,
    duration: Duration,
) {
    tool_session_operation_histogram().record(
        duration.as_millis() as f64,
        &[
            KeyValue::new("operation", operation.to_string()),
            KeyValue::new("tool", tool_name.to_string()),
            KeyValue::new("result", result.to_string()),
        ],
    );
}

// ---------------------------------------------------------------------------
// v2 tool VMs (host/pool.rs, host/grants.rs, host/channel.rs)
// ---------------------------------------------------------------------------

static TOOL_VM_ACTIVE: OnceLock<UpDownCounter<i64>> = OnceLock::new();
static TOOL_VM_CALLS_INFLIGHT: OnceLock<UpDownCounter<i64>> = OnceLock::new();
static TOOL_VM_LIFECYCLE: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_VM_BOOT: OnceLock<Histogram<f64>> = OnceLock::new();
static TOOL_GRANT_DENIED: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_CALL_CPU: OnceLock<Histogram<f64>> = OnceLock::new();
static TOOL_CALL_PEAK_RSS: OnceLock<Histogram<f64>> = OnceLock::new();

fn meter() -> opentelemetry::metrics::Meter {
    global::meter("baml_rt_tools")
}

/// Why a pooled tool VM was evicted. Bounded label values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmEvictReason {
    Idle,
    MaxLifetime,
    Unhealthy,
    Budget,
    Released,
    Shutdown,
}

impl VmEvictReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::MaxLifetime => "max_lifetime",
            Self::Unhealthy => "unhealthy",
            Self::Budget => "budget",
            Self::Released => "released",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Which authority check refused a tool grant. Bounded label values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantDenial {
    /// Bundle bytes failed re-audit against the approved snapshot.
    Audit,
    /// No operator policy for this (agent, tool digest).
    UnauthorizedAgent,
    /// The bundle requests capabilities beyond the agent policy.
    CapabilityOutsidePolicy,
    /// A requested destination is invalid, unresolvable, or forbidden.
    Network,
    /// A requested secret is unbound, unavailable, or misdeclared.
    Secret,
    /// Grant resolution exceeded the control timeout.
    Timeout,
}

impl GrantDenial {
    fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::UnauthorizedAgent => "unauthorized_agent",
            Self::CapabilityOutsidePolicy => "capability_outside_policy",
            Self::Network => "network",
            Self::Secret => "secret",
            Self::Timeout => "timeout",
        }
    }
}

fn tool_vm_active() -> &'static UpDownCounter<i64> {
    TOOL_VM_ACTIVE.get_or_init(|| {
        meter()
            .i64_up_down_counter("baml_rt_tools.tool_vm.active")
            .init()
    })
}

fn tool_vm_calls_inflight() -> &'static UpDownCounter<i64> {
    TOOL_VM_CALLS_INFLIGHT.get_or_init(|| {
        meter()
            .i64_up_down_counter("baml_rt_tools.tool_vm.calls_inflight")
            .init()
    })
}

fn tool_vm_lifecycle() -> &'static Counter<u64> {
    TOOL_VM_LIFECYCLE.get_or_init(|| {
        meter()
            .u64_counter("baml_rt_tools.tool_vm.lifecycle.total")
            .init()
    })
}

fn tool_vm_boot() -> &'static Histogram<f64> {
    TOOL_VM_BOOT.get_or_init(|| {
        meter()
            .f64_histogram("baml_rt_tools.tool_vm.boot.duration_ms")
            .init()
    })
}

fn tool_grant_denied() -> &'static Counter<u64> {
    TOOL_GRANT_DENIED.get_or_init(|| {
        meter()
            .u64_counter("baml_rt_tools.tool_grant.denied.total")
            .init()
    })
}

fn tool_call_cpu() -> &'static Histogram<f64> {
    TOOL_CALL_CPU.get_or_init(|| {
        meter()
            .f64_histogram("baml_rt_tools.tool_call.cpu_ms")
            .init()
    })
}

fn tool_call_peak_rss() -> &'static Histogram<f64> {
    TOOL_CALL_PEAK_RSS.get_or_init(|| {
        meter()
            .f64_histogram("baml_rt_tools.tool_call.peak_rss_kib")
            .init()
    })
}

fn lifecycle(event: &'static str, reason: &'static str, count: u64) {
    tool_vm_lifecycle().add(
        count,
        &[
            KeyValue::new("event", event),
            KeyValue::new("reason", reason),
        ],
    );
}

/// A VM booted and completed the supervisor handshake.
pub(crate) fn record_tool_vm_created(trust_domain: &'static str, boot: Duration) {
    tool_vm_active().add(1, &[KeyValue::new("trust_domain", trust_domain)]);
    lifecycle("created", "cold_boot", 1);
    tool_vm_boot().record(
        boot.as_secs_f64() * 1000.0,
        &[KeyValue::new("result", "ok")],
    );
}

/// VM boot or supervisor handshake failed; nothing joined the pool.
pub(crate) fn record_tool_vm_create_failed(boot: Duration) {
    lifecycle("create_failed", "boot", 1);
    tool_vm_boot().record(
        boot.as_secs_f64() * 1000.0,
        &[KeyValue::new("result", "error")],
    );
}

/// A pooled VM was torn down and left the pool.
pub(crate) fn record_tool_vm_evicted(trust_domain: &'static str, reason: VmEvictReason) {
    tool_vm_active().add(-1, &[KeyValue::new("trust_domain", trust_domain)]);
    lifecycle("evicted", reason.as_str(), 1);
}

/// Teardown failed; the VM stays in the pool (still charged to the budget).
pub(crate) fn record_tool_vm_teardown_failed(reason: VmEvictReason) {
    lifecycle("teardown_failed", reason.as_str(), 1);
}

/// VMs a previous run of this runner left behind, removed at pool start.
pub(crate) fn record_tool_vms_swept(count: usize) {
    if count > 0 {
        lifecycle("swept", "previous_run", count as u64);
    }
}

/// A call holds (+1) or released (-1) a pooled VM. Host-side proxy for busy
/// guest workers: the supervisor does not report its worker count.
pub(crate) fn record_tool_vm_call_inflight(delta: i64) {
    tool_vm_calls_inflight().add(delta, &[]);
}

pub(crate) fn record_tool_grant_denied(reason: GrantDenial) {
    tool_grant_denied().add(1, &[KeyValue::new("reason", reason.as_str())]);
}

/// Per-call resource use the guest supervisor reports in its `usage` event.
pub(crate) fn record_tool_call_usage(cpu_ms: u64, peak_rss_kib: u64) {
    tool_call_cpu().record(cpu_ms as f64, &[]);
    tool_call_peak_rss().record(peak_rss_kib as f64, &[]);
}
