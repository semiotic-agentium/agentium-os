//! OpenTelemetry-oriented tracing spans for the command/effect bus (`baml_rt_core.*` namespace).
//!
//! Keeps span names static and low-cardinality; dynamic data is fields only. See
//! `docs/otel-trace-instrumentation-guide.md`.

use tracing::Span;

/// Root span for one `EffectEmitter::emit` → `process_effect` turn (counts update + subscriber fanout).
#[inline]
pub(crate) fn effect_emit_process(event_variant: &'static str, context_id: &str) -> Span {
    tracing::info_span!(
        "baml_rt_core.effect_emit.process",
        event.variant = event_variant,
        context_id = context_id,
    )
}

/// Child span: synchronous `EffectSubscriber::on_effect` (non–`LlmCompleted` path).
#[inline]
pub(crate) fn effect_emit_subscriber_notify(
    parent: &Span,
    subscriber_slot: u8,
    event_variant: &'static str,
    dispatch_mode: &'static str,
) -> Span {
    tracing::debug_span!(
        parent: parent,
        "baml_rt_core.effect_emit.subscriber_notify",
        subscriber.slot = subscriber_slot,
        event.variant = event_variant,
        dispatch.mode = dispatch_mode,
    )
}

/// Per-envelope fan-out on the command bus (`Bus::emit`).
#[inline]
pub(crate) fn bus_emit_envelope(subscriber_bucket: &'static str) -> Span {
    tracing::trace_span!(
        "baml_rt_core.bus.emit_envelope",
        envelope.subscriber_bucket = subscriber_bucket,
    )
}

fn subscriber_count_bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=4 => "2-4",
        _ => "5+",
    }
}

/// Bucket label for `bus_emit_envelope` from raw subscriber + stream counts.
#[inline]
pub(crate) fn envelope_subscriber_bucket(subscribers: usize, streams: usize) -> &'static str {
    let n = subscribers.saturating_add(streams);
    subscriber_count_bucket(n)
}
