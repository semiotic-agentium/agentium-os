//! OpenTelemetry metrics for the HTTP API surface.
//!
//! Instruments are cached with OnceLock per the metrics guide (no repeated creation on hot path).

use std::{sync::OnceLock, time::Duration};

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

/// Record completion of an HTTP API request (route and result for low cardinality).
pub(crate) fn record_request(route: &str, result: &str, duration: Duration) {
    let attrs = request_attributes(route, result);
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
    use super::request_attributes;

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
}
