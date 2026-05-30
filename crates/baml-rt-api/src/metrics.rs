// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry metrics for the HTTP API surface.
//!
//! Instruments are cached with OnceLock per the metrics guide (no repeated creation on hot path).

use std::{sync::OnceLock, time::Duration};

use axum::{Json, http::StatusCode as AxumStatus};
use baml_rt_core::{AgentInstanceId, AgentPackageName};
use http_api_problem::HttpApiProblem;
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

const METER_NAME: &str = "baml_rt_api";

static REQUEST_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static REQUEST_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static CH_PHASE_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static CH_PAYLOAD_BYTES_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();
static CH_ITEMS_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

fn request_counter() -> &'static Counter<u64> {
    REQUEST_COUNTER.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt_api.http.request_total")
            .init()
    })
}

fn request_histogram() -> &'static Histogram<f64> {
    REQUEST_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt_api.http.request_duration_ms")
            .init()
    })
}

fn request_attributes(route: &str, result: &str) -> [KeyValue; 2] {
    [
        KeyValue::new("route", route.to_string()),
        KeyValue::new("result", result.to_string()),
    ]
}

fn agent_request_attributes(
    route: &str,
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    forwarded: bool,
    ingress_service_instance_id: &str,
    result: &str,
) -> [KeyValue; 6] {
    [
        KeyValue::new("route", route.to_string()),
        KeyValue::new("agent_package", agent_package.as_str().to_string()),
        KeyValue::new("agent_instance_id", agent_instance_id.as_str().to_string()),
        KeyValue::new("forwarded", forwarded.to_string()),
        KeyValue::new(
            "ingress_service_instance_id",
            ingress_service_instance_id.to_string(),
        ),
        KeyValue::new("result", result.to_string()),
    ]
}

fn ch_phase_histogram() -> &'static Histogram<f64> {
    CH_PHASE_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt_api.conversation_history.phase_duration_ms")
            .init()
    })
}

fn ch_payload_bytes_histogram() -> &'static Histogram<f64> {
    CH_PAYLOAD_BYTES_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt_api.conversation_history.payload_bytes")
            .init()
    })
}

fn ch_items_histogram() -> &'static Histogram<f64> {
    CH_ITEMS_HISTOGRAM.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt_api.conversation_history.item_count")
            .init()
    })
}

/// Map RFC 7807 [`HttpApiProblem`] status to a low-cardinality `result` label for `record_request`.
pub(crate) fn http_problem_result_label(problem: &HttpApiProblem) -> &'static str {
    match problem.status.as_ref().map(|s| s.as_u16()) {
        Some(400) => "bad_request",
        Some(401) => "unauthorized",
        Some(404) => "not_found",
        Some(409) => "conflict",
        Some(501) => "unavailable",
        Some(502) => "bad_gateway",
        Some(500) => "internal",
        Some(503) => "unavailable",
        _ => "internal",
    }
}

/// Record metrics for handlers returning `Result<Json<T>, HttpApiProblem>`.
pub(crate) fn finish_json_http_metrics<T>(
    route: &'static str,
    start: std::time::Instant,
    result: &Result<Json<T>, HttpApiProblem>,
) {
    match result {
        Ok(_) => record_request(route, "success", start.elapsed()),
        Err(e) => record_request(route, http_problem_result_label(e), start.elapsed()),
    }
}

/// Record metrics for handlers returning `Result<AxumStatus, HttpApiProblem>`.
pub(crate) fn finish_status_http_metrics(
    route: &'static str,
    start: std::time::Instant,
    result: &Result<AxumStatus, HttpApiProblem>,
) {
    match result {
        Ok(s) if s.is_success() => record_request(route, "success", start.elapsed()),
        Ok(_) => record_request(route, "client_error", start.elapsed()),
        Err(e) => record_request(route, http_problem_result_label(e), start.elapsed()),
    }
}

/// Record completion of an HTTP API request (route and result for low cardinality).
pub(crate) fn record_request(route: &str, result: &str, duration: Duration) {
    let attrs = request_attributes(route, result);
    request_counter().add(1, &attrs);
    request_histogram().record(duration.as_secs_f64() * 1000.0, &attrs);
}

/// Record completion of an agent-scoped HTTP request (`/agents/{package}/{instance}/...`).
///
/// Emits the same counter/histogram as [`record_request`] but adds `agent_package`,
/// `agent_instance_id`, `forwarded`, and `ingress_service_instance_id` labels so
/// operators can slice agent traffic on Grafana without joining on `target_info`.
/// Non-agent routes keep using [`record_request`] so those series don't get agent
/// labels sprayed into them.
///
/// Takes typed identifiers so raw request input cannot become an explicit metric
/// label — the handler must parse first and fall back to [`record_request`] on
/// parse failure.
pub(crate) fn record_agent_http_request(
    route: &str,
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    forwarded: bool,
    ingress_service_instance_id: &str,
    result: &str,
    duration: Duration,
) {
    let attrs = agent_request_attributes(
        route,
        agent_package,
        agent_instance_id,
        forwarded,
        ingress_service_instance_id,
        result,
    );
    request_counter().add(1, &attrs);
    request_histogram().record(duration.as_secs_f64() * 1000.0, &attrs);
}

pub(crate) fn record_conversation_history_phase_duration(phase: &str, duration: Duration) {
    let attrs = [KeyValue::new("phase", phase.to_string())];
    ch_phase_histogram().record(duration.as_secs_f64() * 1000.0, &attrs);
}

pub(crate) fn record_conversation_history_payload(
    event_kind: &str,
    payload_bytes: usize,
    items: usize,
) {
    let attrs = [KeyValue::new("event", event_kind.to_string())];
    ch_payload_bytes_histogram().record(payload_bytes as f64, &attrs);
    ch_items_histogram().record(items as f64, &attrs);
}

#[cfg(test)]
mod tests {
    use baml_rt_core::{AgentInstanceId, AgentPackageName};

    use super::{agent_request_attributes, request_attributes};

    fn demo_identity() -> (AgentPackageName, AgentInstanceId) {
        (
            AgentPackageName::parse("demo-agent").expect("valid package identifier"),
            AgentInstanceId::parse("default").expect("valid instance identifier"),
        )
    }

    #[test]
    fn request_attributes_use_consistent_schema() {
        let attrs = request_attributes("get_mermaid_context", "success");
        let mut got = attrs
            .iter()
            .map(|kv| (kv.key.as_str().to_string(), kv.value.to_string()))
            .collect::<Vec<_>>();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("result".to_string(), "success".to_string()),
                ("route".to_string(), "get_mermaid_context".to_string()),
            ]
        );
    }

    #[test]
    fn agent_request_attributes_carry_identity_and_ingress_pod() {
        let (package, instance) = demo_identity();
        let attrs = agent_request_attributes(
            "post_a2a", &package, &instance, false, "runner-0", "success",
        );
        let mut got = attrs
            .iter()
            .map(|kv| (kv.key.as_str().to_string(), kv.value.to_string()))
            .collect::<Vec<_>>();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("agent_instance_id".to_string(), "default".to_string()),
                ("agent_package".to_string(), "demo-agent".to_string()),
                ("forwarded".to_string(), "false".to_string()),
                (
                    "ingress_service_instance_id".to_string(),
                    "runner-0".to_string(),
                ),
                ("result".to_string(), "success".to_string()),
                ("route".to_string(), "post_a2a".to_string()),
            ]
        );
    }

    #[test]
    fn agent_request_attributes_mark_forwarded_true() {
        let (package, instance) = demo_identity();
        let attrs =
            agent_request_attributes("post_a2a", &package, &instance, true, "runner-1", "success");
        let forwarded = attrs
            .iter()
            .find(|kv| kv.key.as_str() == "forwarded")
            .map(|kv| kv.value.to_string());
        assert_eq!(forwarded.as_deref(), Some("true"));
    }
}
