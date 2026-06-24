// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry metrics helpers.
//!
//! Metrics are defined here to keep instrumentation orthogonal to business logic.

use std::{sync::OnceLock, time::Duration};

use baml_rt_core::{AgentInstanceId, AgentPackageName, InvocationKind};
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

use crate::runner_identity::UNKNOWN_SERVICE_INSTANCE_ID;

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
static LLM_CALL_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static LLM_CALL_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static LLM_PROMPT_BYTES_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static LLM_TOKENS_IN_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static LLM_TOKENS_OUT_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static ONNX_INFERENCE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static ONNX_WAIT_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static ONNX_RUN_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static ONNX_WAIT_RUN_RATIO_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static ONNX_WAIT_DOMINANT_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();

static EVENT_POLL_CYCLE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static EVENT_POLL_CYCLE_DURATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static EVENT_POLL_PRODUCER_OUTCOME_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static EVENT_POLL_PRODUCER_DURATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static EVENT_POLL_EVENTS_PROCESSED_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();

static PROVENANCE_READ_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static PROVENANCE_READ_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

static CLUSTER_A2A_FORWARD_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static CLUSTER_A2A_FORWARD_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

static EVENT_DISPATCH_NO_SUBSCRIBERS_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static EVENT_DISPATCH_OUTCOME_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();

static TASK_DAEMON_RUN_ONCE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static TASK_DAEMON_RUN_ONCE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static MCP_SESSION_EXPIRED_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static MCP_REGISTRY_ENTRY_RECREATED_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static MCP_DIGEST_MISMATCH_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();

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

fn mcp_session_expired_counter() -> &'static Counter<u64> {
    MCP_SESSION_EXPIRED_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("mcp.session_expired_total")
            .init()
    })
}

fn mcp_registry_entry_recreated_counter() -> &'static Counter<u64> {
    MCP_REGISTRY_ENTRY_RECREATED_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("mcp.registry.entry_recreated_total")
            .init()
    })
}

fn mcp_digest_mismatch_counter() -> &'static Counter<u64> {
    MCP_DIGEST_MISMATCH_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("mcp.digest_mismatch_total")
            .init()
    })
}

pub fn mcp_transport_attributes(transport: &str) -> [KeyValue; 1] {
    [KeyValue::new("transport", transport.to_string())]
}

pub fn mcp_digest_mismatch_attributes(kind: &str, transport: &str) -> [KeyValue; 2] {
    [
        KeyValue::new("kind", kind.to_string()),
        KeyValue::new("transport", transport.to_string()),
    ]
}

pub fn record_mcp_session_expired(transport: &str) {
    let attrs = mcp_transport_attributes(transport);
    mcp_session_expired_counter().add(1, &attrs);
}

pub fn record_mcp_registry_entry_recreated(transport: &str) {
    let attrs = mcp_transport_attributes(transport);
    mcp_registry_entry_recreated_counter().add(1, &attrs);
}

pub fn record_mcp_digest_mismatch(kind: &str, transport: &str) {
    let attrs = mcp_digest_mismatch_attributes(kind, transport);
    mcp_digest_mismatch_counter().add(1, &attrs);
}

/// Build the attribute set emitted by [`record_a2a_request`]. Extracted so tests can
/// assert identity labels without touching the global meter.
pub fn a2a_request_attributes(
    method: &str,
    agent_package: &str,
    agent_instance_id: &str,
    result: &str,
    invocation: InvocationKind,
    serving_service_instance_id: &str,
) -> [KeyValue; 6] {
    [
        KeyValue::new("method", method.to_string()),
        KeyValue::new("agent_package", agent_package.to_string()),
        KeyValue::new("agent_instance_id", agent_instance_id.to_string()),
        KeyValue::new("result", result.to_string()),
        KeyValue::new("stream", invocation.is_stream().to_string()),
        KeyValue::new(
            "serving_service_instance_id",
            serving_service_instance_id.to_string(),
        ),
    ]
}

/// Build the attribute set emitted by [`record_a2a_error`]. Extracted so tests can
/// assert identity labels without touching the global meter.
pub fn a2a_error_attributes(
    method: &str,
    agent_package: &str,
    agent_instance_id: &str,
    error_type: &str,
    invocation: InvocationKind,
    serving_service_instance_id: &str,
) -> [KeyValue; 6] {
    [
        KeyValue::new("method", method.to_string()),
        KeyValue::new("agent_package", agent_package.to_string()),
        KeyValue::new("agent_instance_id", agent_instance_id.to_string()),
        KeyValue::new("error_type", error_type.to_string()),
        KeyValue::new("stream", invocation.is_stream().to_string()),
        KeyValue::new(
            "serving_service_instance_id",
            serving_service_instance_id.to_string(),
        ),
    ]
}

/// Record completion of an A2A request.
///
/// `serving_service_instance_id` is the pod-name (OTEL `service.instance.id`) of the
/// runner that served the request, emitted as an explicit label so Grafana can filter
/// without joining on `target_info`.
pub fn record_a2a_request(
    method: &str,
    agent_package: &str,
    agent_instance_id: &str,
    result: &str,
    invocation: InvocationKind,
    serving_service_instance_id: &str,
    duration: Duration,
) {
    let attributes = a2a_request_attributes(
        method,
        agent_package,
        agent_instance_id,
        result,
        invocation,
        serving_service_instance_id,
    );
    a2a_request_counter().add(1, &attributes);
    a2a_request_histogram().record(duration.as_millis() as f64, &attributes);
}

/// Record an A2A error by type.
///
/// `serving_service_instance_id` is the pod-name of the runner that hit the error,
/// matching the label on [`record_a2a_request`].
pub fn record_a2a_error(
    method: &str,
    agent_package: &str,
    agent_instance_id: &str,
    error_type: &str,
    invocation: InvocationKind,
    serving_service_instance_id: &str,
) {
    let attributes = a2a_error_attributes(
        method,
        agent_package,
        agent_instance_id,
        error_type,
        invocation,
        serving_service_instance_id,
    );
    a2a_error_counter().add(1, &attributes);
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

static CONTEXT_COMPACTION_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static CONTEXT_COMPACTION_DURATION_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static CONTEXT_COMPACTION_BYTES_BEFORE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static CONTEXT_COMPACTION_BYTES_AFTER_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static CONTEXT_COMPACTION_COVERED_ROWS_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

fn context_compaction_counter() -> &'static Counter<u64> {
    CONTEXT_COMPACTION_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.context_compaction.attempt_total")
            .init()
    })
}

fn context_compaction_duration_histogram() -> &'static Histogram<f64> {
    CONTEXT_COMPACTION_DURATION_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.context_compaction.duration_ms")
            .init()
    })
}

fn context_compaction_bytes_before_histogram() -> &'static Histogram<f64> {
    CONTEXT_COMPACTION_BYTES_BEFORE_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.context_compaction.prompt_bytes_before")
            .init()
    })
}

fn context_compaction_bytes_after_histogram() -> &'static Histogram<f64> {
    CONTEXT_COMPACTION_BYTES_AFTER_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.context_compaction.prompt_bytes_after")
            .init()
    })
}

fn context_compaction_covered_rows_histogram() -> &'static Histogram<f64> {
    CONTEXT_COMPACTION_COVERED_ROWS_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.context_compaction.covered_rows")
            .init()
    })
}

/// Labels and measurements for a context compaction attempt.
pub struct ContextCompactionMetrics<'a> {
    pub trigger: &'a str,
    pub result: &'a str,
    pub reason: Option<&'a str>,
    pub summarizer_backend: &'a str,
    pub model: &'a str,
    pub provider: &'a str,
    pub budget_source: &'a str,
    pub budget_freshness: &'a str,
    pub duration: Duration,
    pub pre_prompt_bytes: u64,
    pub post_prompt_bytes: u64,
    pub covered_rows: u64,
}

/// Record a host context compaction attempt (post-turn, pre-model emergency, or manual).
pub fn record_context_compaction(metrics: ContextCompactionMetrics<'_>) {
    let ContextCompactionMetrics {
        trigger,
        result,
        reason,
        summarizer_backend,
        model,
        provider,
        budget_source,
        budget_freshness,
        duration,
        pre_prompt_bytes,
        post_prompt_bytes,
        covered_rows,
    } = metrics;
    let mut attributes = vec![
        KeyValue::new("trigger", trigger.to_string()),
        KeyValue::new("result", result.to_string()),
        KeyValue::new("summarizer", summarizer_backend.to_string()),
        KeyValue::new("model", model.to_string()),
        KeyValue::new("provider", provider.to_string()),
        KeyValue::new("budget_source", budget_source.to_string()),
        KeyValue::new("budget_freshness", budget_freshness.to_string()),
    ];
    if let Some(reason) = reason {
        attributes.push(KeyValue::new("reason", reason.to_string()));
    }
    let attributes = attributes.as_slice();
    context_compaction_counter().add(1, attributes);
    context_compaction_duration_histogram().record(duration.as_millis() as f64, attributes);
    if result == "success" {
        context_compaction_bytes_before_histogram().record(pre_prompt_bytes as f64, attributes);
        context_compaction_bytes_after_histogram().record(post_prompt_bytes as f64, attributes);
        context_compaction_covered_rows_histogram().record(covered_rows as f64, attributes);
    }
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

fn provenance_read_counter() -> &'static Counter<u64> {
    PROVENANCE_READ_COUNTER.get_or_init(|| {
        global::meter("baml_rt_provenance")
            .u64_counter("baml_rt_provenance.read.operation_total")
            .init()
    })
}

fn provenance_read_histogram() -> &'static Histogram<f64> {
    PROVENANCE_READ_HISTOGRAM.get_or_init(|| {
        global::meter("baml_rt_provenance")
            .f64_histogram("baml_rt_provenance.read.duration_ms")
            .init()
    })
}

/// Heavy provenance graph read (export, list contexts). `operation` is low-cardinality.
pub fn record_provenance_read(operation: &str, result: &str, duration: Duration) {
    let attributes = &[
        KeyValue::new("operation", operation.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    provenance_read_counter().add(1, attributes);
    provenance_read_histogram().record(duration.as_secs_f64() * 1000.0, attributes);
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

fn event_poll_cycle_counter() -> &'static Counter<u64> {
    EVENT_POLL_CYCLE_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.event_poll.cycle_total")
            .init()
    })
}

fn event_poll_cycle_duration_histogram() -> &'static Histogram<f64> {
    EVENT_POLL_CYCLE_DURATION_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.event_poll.cycle_duration_ms")
            .init()
    })
}

fn event_poll_producer_outcome_counter() -> &'static Counter<u64> {
    EVENT_POLL_PRODUCER_OUTCOME_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.event_poll.producer_outcome_total")
            .init()
    })
}

fn event_poll_producer_duration_histogram() -> &'static Histogram<f64> {
    EVENT_POLL_PRODUCER_DURATION_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.a2a.event_poll.producer_duration_ms")
            .init()
    })
}

fn event_poll_events_processed_counter() -> &'static Counter<u64> {
    EVENT_POLL_EVENTS_PROCESSED_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.event_poll.events_processed_total")
            .init()
    })
}

/// One full event-dispatcher poll sweep (all registered producers).
pub fn record_event_poll_cycle(duration: Duration) {
    event_poll_cycle_counter().add(1, &[]);
    event_poll_cycle_duration_histogram().record(duration.as_secs_f64() * 1000.0, &[]);
}

/// Per-producer poll outcome. `outcome` is low-cardinality (`empty`, `poll_error`, `delivery_error`,
/// `validation_error`, `partial_rejection`, `success`). `producer_key` must stay a bounded registry key.
pub fn record_event_poll_producer(
    producer_key: &str,
    outcome: &str,
    duration: Duration,
    events_processed: u64,
) {
    let attrs = &[
        KeyValue::new("producer_key", producer_key.to_string()),
        KeyValue::new("outcome", outcome.to_string()),
    ];
    event_poll_producer_outcome_counter().add(1, attrs);
    event_poll_producer_duration_histogram().record(duration.as_secs_f64() * 1000.0, attrs);
    if events_processed > 0 {
        let key_only = &[KeyValue::new("producer_key", producer_key.to_string())];
        event_poll_events_processed_counter().add(events_processed, key_only);
    }
}

fn cluster_a2a_forward_counter() -> &'static Counter<u64> {
    CLUSTER_A2A_FORWARD_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.cluster.a2a_forward_total")
            .init()
    })
}

fn cluster_a2a_forward_histogram() -> &'static Histogram<f64> {
    CLUSTER_A2A_FORWARD_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.cluster.a2a_forward_duration_ms")
            .init()
    })
}

/// Build the attribute set emitted by [`record_cluster_a2a_forward`]. Extracted so
/// tests can assert identity labels without touching the global meter.
pub fn cluster_a2a_forward_attributes(
    agent_package: &str,
    agent_instance_id: &str,
    result: &str,
    ingress_service_instance_id: &str,
    target_service_instance_id: Option<&str>,
) -> [KeyValue; 5] {
    [
        KeyValue::new("agent_package", agent_package.to_string()),
        KeyValue::new("agent_instance_id", agent_instance_id.to_string()),
        KeyValue::new("result", result.to_string()),
        KeyValue::new(
            "ingress_service_instance_id",
            ingress_service_instance_id.to_string(),
        ),
        KeyValue::new(
            "target_service_instance_id",
            target_service_instance_id
                .unwrap_or(UNKNOWN_SERVICE_INSTANCE_ID)
                .to_string(),
        ),
    ]
}

/// Cross-runner HTTP A2A forward (cluster placement).
///
/// Identity labels follow the same low-cardinality contract as the ingress HTTP
/// and A2A serving families: agent identity is typed (parsed before the ingress
/// handler reaches this call site); `ingress_service_instance_id` and
/// `target_service_instance_id` carry the pod-shaped `service.instance.id`s of
/// the originating and destination runners respectively. `result` is drawn from
/// the bounded set `success` / `http_error` / `transport_error` / `parse_error`
/// / `invalid_argument` / `error` (see `cluster_forward_error_label`).
pub fn record_cluster_a2a_forward(
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    result: &str,
    ingress_service_instance_id: &str,
    target_service_instance_id: Option<&str>,
    duration: Duration,
) {
    let attrs = cluster_a2a_forward_attributes(
        agent_package.as_str(),
        agent_instance_id.as_str(),
        result,
        ingress_service_instance_id,
        target_service_instance_id,
    );
    cluster_a2a_forward_counter().add(1, &attrs);
    cluster_a2a_forward_histogram().record(duration.as_secs_f64() * 1000.0, &attrs);
}

fn event_dispatch_no_subscribers_counter() -> &'static Counter<u64> {
    EVENT_DISPATCH_NO_SUBSCRIBERS_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.event_dispatch.no_subscribers_total")
            .init()
    })
}

fn event_dispatch_outcome_counter() -> &'static Counter<u64> {
    EVENT_DISPATCH_OUTCOME_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.a2a.event_dispatch.subscriber_delivery_total")
            .init()
    })
}

/// Produced event had no matching agent subscriptions (`producer_key` must stay bounded).
pub fn record_event_dispatch_no_subscribers(producer_key: &str) {
    let attrs = &[KeyValue::new("producer_key", producer_key.to_string())];
    event_dispatch_no_subscribers_counter().add(1, attrs);
}

/// After attempting delivery to matching subscribers. `outcome`: `all_accepted`, `partial_rejection`, `all_rejected`.
pub fn record_event_dispatch_subscriber_batch(
    producer_key: &str,
    subscribers_matched: usize,
    outcome: &str,
) {
    let bucket = match subscribers_matched {
        0 => "0",
        1 => "1",
        _ => "many",
    };
    let attrs = &[
        KeyValue::new("producer_key", producer_key.to_string()),
        KeyValue::new("subscribers_bucket", bucket.to_string()),
        KeyValue::new("outcome", outcome.to_string()),
    ];
    event_dispatch_outcome_counter().add(1, attrs);
}

const TASK_DAEMON_METER: &str = "baml_rt_task_daemon";

fn task_daemon_run_once_counter() -> &'static Counter<u64> {
    TASK_DAEMON_RUN_ONCE_COUNTER.get_or_init(|| {
        global::meter(TASK_DAEMON_METER)
            .u64_counter("baml_rt_task_daemon.run_once.total")
            .init()
    })
}

fn task_daemon_run_once_histogram() -> &'static Histogram<f64> {
    TASK_DAEMON_RUN_ONCE_HISTOGRAM.get_or_init(|| {
        global::meter(TASK_DAEMON_METER)
            .f64_histogram("baml_rt_task_daemon.run_once.duration_ms")
            .init()
    })
}

/// One task-daemon poll / extract / deliver iteration. `source_kind`: `slack`, `clickup`, etc.
pub fn record_task_daemon_run_once(source_kind: &str, result: &str, duration: Duration) {
    let attrs = &[
        KeyValue::new("source_kind", source_kind.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    task_daemon_run_once_counter().add(1, attrs);
    task_daemon_run_once_histogram().record(duration.as_secs_f64() * 1000.0, attrs);
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

fn llm_call_counter() -> &'static Counter<u64> {
    LLM_CALL_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.llm.call_total")
            .init()
    })
}

fn llm_call_histogram() -> &'static Histogram<f64> {
    LLM_CALL_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.llm.call_duration_ms")
            .init()
    })
}

fn llm_prompt_bytes_histogram() -> &'static Histogram<f64> {
    LLM_PROMPT_BYTES_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.llm.prompt_bytes")
            .init()
    })
}

fn llm_tokens_in_counter() -> &'static Counter<u64> {
    LLM_TOKENS_IN_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.llm.tokens_in_total")
            .init()
    })
}

fn llm_tokens_out_counter() -> &'static Counter<u64> {
    LLM_TOKENS_OUT_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.llm.tokens_out_total")
            .init()
    })
}

fn onnx_inference_counter() -> &'static Counter<u64> {
    ONNX_INFERENCE_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.onnx.inference_total")
            .init()
    })
}

fn onnx_wait_histogram() -> &'static Histogram<f64> {
    ONNX_WAIT_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.onnx.wait_ms")
            .init()
    })
}

fn onnx_run_histogram() -> &'static Histogram<f64> {
    ONNX_RUN_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.onnx.run_ms")
            .init()
    })
}

fn onnx_wait_run_ratio_histogram() -> &'static Histogram<f64> {
    ONNX_WAIT_RUN_RATIO_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt.onnx.wait_to_run_ratio")
            .init()
    })
}

fn onnx_wait_dominant_counter() -> &'static Counter<u64> {
    ONNX_WAIT_DOMINANT_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt.onnx.wait_dominant_total")
            .init()
    })
}

/// Metrics collected for a single LLM call.
pub struct LlmCallMetrics<'a> {
    pub function_name: &'a str,
    pub client: &'a str,
    pub model: &'a str,
    pub result: &'a str,
    pub duration: Duration,
    pub prompt_bytes: usize,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
}

/// Record LLM call timing, prompt payload size, and optional token usage.
pub fn record_llm_call(m: &LlmCallMetrics<'_>) {
    let attributes = &[
        KeyValue::new("function", m.function_name.to_string()),
        KeyValue::new("client", m.client.to_string()),
        KeyValue::new("model", m.model.to_string()),
        KeyValue::new("result", m.result.to_string()),
    ];

    llm_call_counter().add(1, attributes);
    llm_call_histogram().record(m.duration.as_millis() as f64, attributes);
    llm_prompt_bytes_histogram().record(m.prompt_bytes as f64, attributes);

    if let Some(v) = m.tokens_in {
        llm_tokens_in_counter().add(v, attributes);
    }
    if let Some(v) = m.tokens_out {
        llm_tokens_out_counter().add(v, attributes);
    }
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

/// Record ONNX inference queueing vs execution timings.
///
/// `operation` is low-cardinality (e.g. `embed_batch`, `rerank_pair`, `citation_drift`).
/// A wait is considered dominant when `wait_ms >= run_ms` and `run_ms > 0`.
pub fn record_onnx_inference(operation: &'static str, wait: Duration, run: Duration) {
    let wait_ms = wait.as_secs_f64() * 1000.0;
    let run_ms = run.as_secs_f64() * 1000.0;
    let ratio = if run_ms > 0.0 { wait_ms / run_ms } else { 0.0 };
    let wait_dominant = run_ms > 0.0 && wait_ms >= run_ms;

    let attributes = &[KeyValue::new("operation", operation.to_string())];
    onnx_inference_counter().add(1, attributes);
    onnx_wait_histogram().record(wait_ms, attributes);
    onnx_run_histogram().record(run_ms, attributes);
    onnx_wait_run_ratio_histogram().record(ratio, attributes);
    if wait_dominant {
        onnx_wait_dominant_counter().add(1, attributes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr_value(attrs: &[KeyValue], key: &str) -> Option<String> {
        attrs
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    #[test]
    fn a2a_request_attributes_carry_agent_identity_and_serving_pod() {
        let attrs = a2a_request_attributes(
            "message/send",
            "demo-agent",
            "default",
            "success",
            InvocationKind::Invoke,
            "runner-0",
        );
        assert_eq!(
            attr_value(&attrs, "agent_package").as_deref(),
            Some("demo-agent"),
        );
        assert_eq!(
            attr_value(&attrs, "agent_instance_id").as_deref(),
            Some("default"),
        );
        assert_eq!(
            attr_value(&attrs, "serving_service_instance_id").as_deref(),
            Some("runner-0"),
        );
        assert_eq!(
            attr_value(&attrs, "method").as_deref(),
            Some("message/send")
        );
        assert_eq!(attr_value(&attrs, "result").as_deref(), Some("success"));
        assert_eq!(attr_value(&attrs, "stream").as_deref(), Some("false"));
    }

    #[test]
    fn a2a_request_attributes_carry_stream_flag() {
        let attrs = a2a_request_attributes(
            "message/stream",
            "demo-agent",
            "staging",
            "success",
            InvocationKind::Stream,
            "runner-1",
        );
        assert_eq!(attr_value(&attrs, "stream").as_deref(), Some("true"));
        assert_eq!(
            attr_value(&attrs, "agent_instance_id").as_deref(),
            Some("staging"),
        );
    }

    #[test]
    fn a2a_error_attributes_carry_agent_identity_and_serving_pod() {
        let attrs = a2a_error_attributes(
            "message/send",
            "demo-agent",
            "default",
            "agent_not_found",
            InvocationKind::Invoke,
            "runner-0",
        );
        assert_eq!(
            attr_value(&attrs, "agent_package").as_deref(),
            Some("demo-agent"),
        );
        assert_eq!(
            attr_value(&attrs, "agent_instance_id").as_deref(),
            Some("default"),
        );
        assert_eq!(
            attr_value(&attrs, "error_type").as_deref(),
            Some("agent_not_found"),
        );
        assert_eq!(
            attr_value(&attrs, "serving_service_instance_id").as_deref(),
            Some("runner-0"),
        );
    }
}
