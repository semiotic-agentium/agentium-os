//! OpenTelemetry metrics helpers.
//!
//! Metrics are defined here to keep instrumentation orthogonal to business logic.

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
use std::sync::OnceLock;
use std::time::Duration;

const METER_NAME: &str = "baml_rt";

static A2A_REQUEST_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_REQUEST_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static A2A_ERROR_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_STREAM_CHUNK_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_STREAM_CHUNK_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static TOOL_INVOCATION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_INVOCATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

fn a2a_request_counter() -> &'static Counter<u64> {
    A2A_REQUEST_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.request_total")
            .init()
    })
}

fn a2a_request_histogram() -> &'static Histogram<f64> {
    A2A_REQUEST_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.request_duration_ms")
            .init()
    })
}

fn a2a_error_counter() -> &'static Counter<u64> {
    A2A_ERROR_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.error_total")
            .init()
    })
}

fn a2a_stream_chunk_counter() -> &'static Counter<u64> {
    A2A_STREAM_CHUNK_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.stream.chunk_total")
            .init()
    })
}

fn a2a_stream_chunk_histogram() -> &'static Histogram<f64> {
    A2A_STREAM_CHUNK_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.stream.chunk_count")
            .init()
    })
}

fn tool_invocation_counter() -> &'static Counter<u64> {
    TOOL_INVOCATION_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.tool.invocation_total")
            .init()
    })
}

fn tool_invocation_histogram() -> &'static Histogram<f64> {
    TOOL_INVOCATION_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.tool.invocation_duration_ms")
            .init()
    })
}

/// Record completion of an A2A request.
pub fn record_a2a_request(method: &str, result: &str, is_stream: bool, duration: Duration) {
    let attributes = &[
        KeyValue::new("method", method.to_string()),
        KeyValue::new("result", result.to_string()),
        KeyValue::new("stream", is_stream.to_string()),
    ];

    a2a_request_counter().add(1, attributes);
    a2a_request_histogram().record(duration.as_millis() as f64, attributes);
}

/// Record an A2A error by type.
pub fn record_a2a_error(method: &str, error_type: &str, is_stream: bool) {
    let attributes = &[
        KeyValue::new("method", method.to_string()),
        KeyValue::new("error_type", error_type.to_string()),
        KeyValue::new("stream", is_stream.to_string()),
    ];
    a2a_error_counter().add(1, attributes);
}

/// Record the number of chunks produced by a stream.
pub fn record_a2a_stream_chunks(method: &str, chunk_count: usize) {
    let attributes = &[KeyValue::new("method", method.to_string())];
    a2a_stream_chunk_counter().add(chunk_count as u64, attributes);
    a2a_stream_chunk_histogram().record(chunk_count as f64, attributes);
}

/// Record tool invocation metrics.
pub fn record_tool_invocation(tool_name: &str, result: &str, duration: Duration) {
    let attributes = &[
        KeyValue::new("tool", tool_name.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    tool_invocation_counter().add(1, attributes);
    tool_invocation_histogram().record(duration.as_millis() as f64, attributes);
}
