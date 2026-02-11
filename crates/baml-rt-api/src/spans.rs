//! OpenTelemetry span helpers for the HTTP API surface.
//!
//! Instrumentation is kept orthogonal to handlers per the trace guide.
//! All span names use the `baml_rt_api.` namespace for low cardinality.

use tracing::Span;

/// Create span for GET /agents (list running agents).
///
/// Parent: HTTP request span (from middleware when present).
#[inline]
pub(crate) fn list_agents() -> Span {
    tracing::debug_span!("baml_rt_api.list_agents")
}

/// Create span for POST /agents/.../a2a (JSON-RPC forward).
///
/// Parent: HTTP request span.
#[inline]
pub(crate) fn post_a2a(agent_package: &str, agent_instance_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_api.post_a2a",
        agent_package = %agent_package,
        agent_instance_id = %agent_instance_id,
    )
}

/// Create span for POST /agents/.../a2a/sse (SSE stream).
///
/// Parent: HTTP request span.
#[inline]
pub(crate) fn post_a2a_sse(agent_package: &str, agent_instance_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_api.post_a2a_sse",
        agent_package = %agent_package,
        agent_instance_id = %agent_instance_id,
    )
}
