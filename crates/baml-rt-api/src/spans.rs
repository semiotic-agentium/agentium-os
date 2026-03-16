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

/// Create span for POST /agents/.../dispatch (deterministic buffered delivery).
///
/// Parent: HTTP request span.
#[inline]
pub(crate) fn post_dispatch(agent_package: &str, agent_instance_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_api.post_dispatch",
        agent_package = %agent_package,
        agent_instance_id = %agent_instance_id,
    )
}

/// Create span for GET /contexts/{context_id}/mermaid.
#[inline]
pub(crate) fn get_mermaid_context(context_id: &str) -> Span {
    tracing::debug_span!("baml_rt_api.get_mermaid_context", context_id = %context_id)
}

/// Create span for GET /tasks/{task_id}/mermaid.
#[inline]
pub(crate) fn get_mermaid_task(task_id: &str) -> Span {
    tracing::debug_span!("baml_rt_api.get_mermaid_task", task_id = %task_id)
}

/// Create span for GET /contexts/{context_id}/metrics.
#[inline]
pub(crate) fn get_context_metrics(context_id: &str) -> Span {
    tracing::debug_span!("baml_rt_api.get_context_metrics", context_id = %context_id)
}

/// Create span for GET /contexts/{context_id}/planning.
#[inline]
pub(crate) fn get_context_planning(context_id: &str) -> Span {
    tracing::debug_span!("baml_rt_api.get_context_planning", context_id = %context_id)
}

#[inline]
pub(crate) fn get_provenance_llm_calls() -> Span {
    tracing::debug_span!("baml_rt_api.get_provenance_llm_calls")
}

#[inline]
pub(crate) fn get_provenance_tool_calls() -> Span {
    tracing::debug_span!("baml_rt_api.get_provenance_tool_calls")
}

#[inline]
pub(crate) fn get_provenance_messages() -> Span {
    tracing::debug_span!("baml_rt_api.get_provenance_messages")
}

#[inline]
pub(crate) fn get_provenance_aggregates() -> Span {
    tracing::debug_span!("baml_rt_api.get_provenance_aggregates")
}
