//! OpenTelemetry metrics helpers.
//!
//! Metrics are defined here to keep instrumentation orthogonal to business logic.

use std::{sync::OnceLock, time::Duration};

use baml_rt_core::InvocationKind;
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

const METER_NAME: &str = "baml_rt";

static A2A_REQUEST_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_REQUEST_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static A2A_ERROR_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_STREAM_CHUNK_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_STREAM_CHUNK_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static TOOL_INVOCATION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TOOL_INVOCATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static PROVENANCE_WRITE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static PROVENANCE_WRITE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static A2A_WORKER_HANDLE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static A2A_WORKER_HANDLE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static TASK_STORE_OP_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TASK_STORE_OP_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static QUICKJS_INVOKE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static QUICKJS_INVOKE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static LIVE_STREAM_EVENT_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static LIVE_STREAM_PHASE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static A2A_SSE_STREAM_TO_FIRST_DATA_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static A2A_SSE_TTFB_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

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
pub fn record_a2a_request(
    method: &str,
    result: &str,
    invocation: InvocationKind,
    duration: Duration,
) {
    let attributes = &[
        KeyValue::new("method", method.to_string()),
        KeyValue::new("result", result.to_string()),
        KeyValue::new("stream", invocation.is_stream().to_string()),
    ];

    a2a_request_counter().add(1, attributes);
    a2a_request_histogram().record(duration.as_millis() as f64, attributes);
}

/// Record an A2A error by type.
pub fn record_a2a_error(method: &str, error_type: &str, invocation: InvocationKind) {
    let attributes = &[
        KeyValue::new("method", method.to_string()),
        KeyValue::new("error_type", error_type.to_string()),
        KeyValue::new("stream", invocation.is_stream().to_string()),
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

fn provenance_write_counter() -> &'static Counter<u64> {
    PROVENANCE_WRITE_COUNTER.get_or_init(|| {
        global::meter("baml_rt_provenance")
            .u64_counter("baml_rt_provenance.event.write_total")
            .init()
    })
}

fn provenance_write_histogram() -> &'static Histogram<f64> {
    PROVENANCE_WRITE_HISTOGRAM.get_or_init(|| {
        global::meter("baml_rt_provenance")
            .f64_histogram("baml_rt_provenance.event.write_duration_ms")
            .init()
    })
}

/// Record a single provenance event write.
pub fn record_provenance_write(event_kind: &str, result: &str, duration: Duration) {
    let attributes = &[
        KeyValue::new("event_kind", event_kind.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    provenance_write_counter().add(1, attributes);
    provenance_write_histogram().record(duration.as_millis() as f64, attributes);
}

static PROVENANCE_SEQUENCE_RENDER_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static PROVENANCE_SEQUENCE_RENDER_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

fn provenance_sequence_render_counter() -> &'static Counter<u64> {
    PROVENANCE_SEQUENCE_RENDER_COUNTER.get_or_init(|| {
        global::meter("baml_rt_provenance")
            .u64_counter("baml_rt_provenance.sequence.render_total")
            .init()
    })
}

fn provenance_sequence_render_histogram() -> &'static Histogram<f64> {
    PROVENANCE_SEQUENCE_RENDER_HISTOGRAM.get_or_init(|| {
        global::meter("baml_rt_provenance")
            .f64_histogram("baml_rt_provenance.sequence.render_duration_ms")
            .init()
    })
}

/// Record sequence diagram render (graph → Mermaid).
/// Scope: "context" | "task" | "full". Nodes bucket for low cardinality.
pub fn record_provenance_sequence_render(scope: &str, duration: Duration, nodes_count: usize) {
    let nodes_bucket = match nodes_count {
        0..=10 => "0-10",
        11..=50 => "11-50",
        51..=100 => "51-100",
        _ => "100+",
    };
    let attributes = &[
        KeyValue::new("scope", scope.to_string()),
        KeyValue::new("nodes_bucket", nodes_bucket),
    ];
    provenance_sequence_render_counter().add(1, attributes);
    provenance_sequence_render_histogram().record(duration.as_millis() as f64, attributes);
}

fn a2a_worker_handle_counter() -> &'static Counter<u64> {
    A2A_WORKER_HANDLE_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.worker.handle_total")
            .init()
    })
}

fn a2a_worker_handle_histogram() -> &'static Histogram<f64> {
    A2A_WORKER_HANDLE_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.worker.handle_duration_ms")
            .init()
    })
}

/// Record session runtime worker handle completion.
pub fn record_a2a_worker_handle(result: &str, duration: Duration) {
    let attributes = &[KeyValue::new("result", result.to_string())];
    a2a_worker_handle_counter().add(1, attributes);
    a2a_worker_handle_histogram().record(duration.as_millis() as f64, attributes);
}

fn task_store_op_counter() -> &'static Counter<u64> {
    TASK_STORE_OP_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.task_store.operation_total")
            .init()
    })
}

fn task_store_op_histogram() -> &'static Histogram<f64> {
    TASK_STORE_OP_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.task_store.operation_duration_ms")
            .init()
    })
}

/// Record task store operation.
pub fn record_task_store_operation(operation: &str, result: &str, duration: Duration) {
    let attributes = &[
        KeyValue::new("operation", operation.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    task_store_op_counter().add(1, attributes);
    task_store_op_histogram().record(duration.as_millis() as f64, attributes);
}

fn quickjs_invoke_counter() -> &'static Counter<u64> {
    QUICKJS_INVOKE_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.quickjs.invoke_total")
            .init()
    })
}

fn quickjs_invoke_histogram() -> &'static Histogram<f64> {
    QUICKJS_INVOKE_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.quickjs.invoke_duration_ms")
            .init()
    })
}

/// Record QuickJS invocation (stream or non-stream).
pub fn record_quickjs_invoke(mode: &str, result: &str, duration: Duration) {
    let attributes = &[
        KeyValue::new("mode", mode.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    quickjs_invoke_counter().add(1, attributes);
    quickjs_invoke_histogram().record(duration.as_millis() as f64, attributes);
}

fn live_stream_event_counter() -> &'static Counter<u64> {
    LIVE_STREAM_EVENT_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.live_stream.event_total")
            .init()
    })
}

fn live_stream_phase_histogram() -> &'static Histogram<f64> {
    LIVE_STREAM_PHASE_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.live_stream.phase_duration_ms")
            .init()
    })
}

fn a2a_sse_stream_to_first_data_histogram() -> &'static Histogram<f64> {
    A2A_SSE_STREAM_TO_FIRST_DATA_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.sse.first_data_from_stream_ms")
            .init()
    })
}

fn a2a_sse_ttfb_histogram() -> &'static Histogram<f64> {
    A2A_SSE_TTFB_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.sse.ttfb_from_handler_entry_ms")
            .init()
    })
}

/// One-shot events on the HTTP `message.sendStream` live path (`event` is low-cardinality).
pub fn record_live_stream_event(event: &'static str) {
    let attributes = &[KeyValue::new("event", event.to_string())];
    live_stream_event_counter().add(1, attributes);
}

/// Elapsed time between milestones on the live stream path (`phase` is low-cardinality).
pub fn record_live_stream_phase_duration(phase: &'static str, duration: Duration) {
    let attributes = &[KeyValue::new("phase", phase.to_string())];
    live_stream_phase_histogram().record(duration.as_secs_f64() * 1000.0, attributes);
}

/// Time from successful `handle_a2a_stream` return until the first bus chunk is mapped to an SSE `data:` event.
pub fn record_a2a_sse_first_data_duration_ms(duration: Duration) {
    a2a_sse_stream_to_first_data_histogram().record(duration.as_secs_f64() * 1000.0, &[]);
}

/// Time from HTTP handler entry until the first application SSE data event is ready (includes `handle_a2a_stream` await).
pub fn record_a2a_sse_ttfb_from_handler_entry_ms(duration: Duration) {
    a2a_sse_ttfb_histogram().record(duration.as_secs_f64() * 1000.0, &[]);
}
