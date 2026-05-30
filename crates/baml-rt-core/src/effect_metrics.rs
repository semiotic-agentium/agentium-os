// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry metrics for effect bus processing (`baml_rt_core.*` meter).
//!
//! Follows `docs/otel-metrics-instrumentation-guide.md`: static names, attributes for variants,
//! `OnceLock` for instruments.

use std::{sync::OnceLock, time::Duration};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

const METER_NAME: &str = "baml_rt_core";

static EFFECT_PROCESS_MS: OnceLock<Histogram<f64>> = OnceLock::new();
static EFFECT_SUBSCRIBER_MS: OnceLock<Histogram<f64>> = OnceLock::new();
static EFFECT_SUBSCRIBER_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

fn effect_process_ms() -> &'static Histogram<f64> {
    EFFECT_PROCESS_MS.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt_core.effect_emit.process_duration_ms")
            .init()
    })
}

fn effect_subscriber_ms() -> &'static Histogram<f64> {
    EFFECT_SUBSCRIBER_MS.get_or_init(|| {
        global::meter(METER_NAME)
            .f64_histogram("baml_rt_core.effect_emit.subscriber_duration_ms")
            .init()
    })
}

fn effect_subscriber_total() -> &'static Counter<u64> {
    EFFECT_SUBSCRIBER_TOTAL.get_or_init(|| {
        global::meter(METER_NAME)
            .u64_counter("baml_rt_core.effect_emit.subscriber_notify_total")
            .init()
    })
}

/// End-to-end `process_effect` (liveness map + subscribers).
pub fn record_effect_process(event_variant: &'static str, duration: Duration) {
    let attrs = &[KeyValue::new("event.variant", event_variant)];
    effect_process_ms().record(duration.as_millis() as f64, attrs);
}

/// One subscriber invocation (`on_effect`). Increments
/// `baml_rt_core.effect_emit.subscriber_notify_total` and records latency on
/// `baml_rt_core.effect_emit.subscriber_duration_ms` with the same attribute set.
///
/// The `subscriber` attribute carries the subscriber's stable, low-cardinality
/// identity (e.g. `"provenance"`, `"auto_status"`) and is the canonical alert
/// dimension for failures, paired with `result="error"` and `event.variant`.
pub fn record_effect_subscriber(
    event_variant: &'static str,
    dispatch_mode: &'static str,
    subscriber: &'static str,
    result: &'static str,
    duration: Duration,
) {
    let attrs = &[
        KeyValue::new("event.variant", event_variant),
        KeyValue::new("dispatch.mode", dispatch_mode),
        KeyValue::new("subscriber", subscriber),
        KeyValue::new("result", result),
    ];
    effect_subscriber_total().add(1, attrs);
    effect_subscriber_ms().record(duration.as_millis() as f64, attrs);
}
