//! OpenTelemetry metrics instrumentation for tool operations.
//!
//! This module provides orthogonal metrics recording helpers following the pattern
//! from the OpenTelemetry metrics instrumentation guide. All metrics use static
//! names with dynamic data in structured attributes. Instruments are cached
//! using OnceLock for zero-allocation hot paths.

use std::{sync::OnceLock, time::Duration};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

// Static caches - initialized once, reused forever
static TOOL_REGISTRATION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_EXECUTION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_EXECUTION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
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

fn tool_execution_counter() -> &'static Counter<u64> {
    TOOL_EXECUTION_COUNTER.get_or_init(|| {
        global::meter("baml_rt_tools")
            .u64_counter("baml_rt_tools.tool.execution.total")
            .init()
    })
}

fn tool_execution_histogram() -> &'static Histogram<f64> {
    TOOL_EXECUTION_HISTOGRAM.get_or_init(|| {
        global::meter("baml_rt_tools")
            .f64_histogram("baml_rt_tools.tool.execution.duration_ms")
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

/// Record tool execution with duration and result.
pub(crate) fn record_tool_execution(tool_name: &str, result: &str, duration: Duration) {
    let attributes = &[
        KeyValue::new("tool", tool_name.to_string()),
        KeyValue::new("result", result.to_string()),
    ];

    tool_execution_counter().add(1, attributes);
    tool_execution_histogram().record(duration.as_millis() as f64, attributes);
}

/// Record tool session open event.
pub(crate) fn record_session_open(tool_name: &str) {
    tool_session_open_counter().add(1, &[KeyValue::new("tool", tool_name.to_string())]);
}

/// Record tool session operation (send, next, finish) with duration.
pub(crate) fn record_session_operation(operation: &str, duration: Duration) {
    tool_session_operation_histogram().record(
        duration.as_millis() as f64,
        &[KeyValue::new("operation", operation.to_string())],
    );
}
