//! OpenTelemetry span helpers for GitHub REST client calls.
//!
//! Orthogonal to request building per the OTel instrumentation guide.

use tracing::Span;

/// Span for [`super::GitHubClient::send_json`]: outbound JSON request / response handling.
///
/// Record `url` after building the request when available.
#[inline]
pub(crate) fn send_json() -> Span {
    tracing::debug_span!(
        "integrations_github_client.send_json",
        url = tracing::field::Empty,
    )
}
