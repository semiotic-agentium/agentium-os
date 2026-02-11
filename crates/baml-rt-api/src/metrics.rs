//! OpenTelemetry metrics for the HTTP API surface.
//!
//! Instruments are cached with OnceLock per the metrics guide (no repeated creation on hot path).

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};
use std::sync::OnceLock;
use std::time::Duration;

const METER_NAME: &str = "baml_rt_api";

static REQUEST_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static REQUEST_HISTOGRAM: OnceLock<Histogram<f64>> = OnceLock::new();

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

/// Record completion of an HTTP API request (route and result for low cardinality).
pub(crate) fn record_request(route: &str, result: &str, duration: Duration) {
    let attrs = &[
        KeyValue::new("route", route.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    request_counter().add(1, attrs);
    request_histogram().record(duration.as_secs_f64() * 1000.0, attrs);
}
