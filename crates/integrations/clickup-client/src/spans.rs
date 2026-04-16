//! OpenTelemetry span helpers for ClickUp REST client calls.

use tracing::Span;

/// Span for [`super::ClickUpClient::send_json`].
#[inline]
pub(crate) fn send_json() -> Span {
    tracing::debug_span!(
        "integrations_clickup_client.send_json",
        url = tracing::field::Empty,
    )
}
