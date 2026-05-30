// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry-oriented tracing spans for the effect bus (`baml_rt_core.*` namespace).
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

/// Child span: per-subscriber `EffectSubscriber::on_effect` notify. The `subscriber`
/// field is the stable identity returned by `EffectSubscriber::name()`.
#[inline]
pub(crate) fn effect_emit_subscriber_notify(
    parent: &Span,
    subscriber: &'static str,
    event_variant: &'static str,
    dispatch_mode: &'static str,
) -> Span {
    tracing::debug_span!(
        parent: parent,
        "baml_rt_core.effect_emit.subscriber_notify",
        subscriber = subscriber,
        event.variant = event_variant,
        dispatch.mode = dispatch_mode,
    )
}
