// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry span helpers for the HTTP API surface.
//!
//! Instrumentation is kept orthogonal to handlers per the trace guide.
//! All span names use the `baml_rt_api.` namespace for low cardinality.

use baml_rt_core::{AgentInstanceId, AgentPackageName};
use baml_rt_observability::UNKNOWN_SERVICE_INSTANCE_ID;
use tracing::Span;

/// Create span for GET /agents (list running agents).
///
/// Parent: HTTP request span (from middleware when present).
#[inline]
pub(crate) fn list_agents() -> Span {
    tracing::debug_span!("baml_rt_api.list_agents")
}

/// Create span for POST /agents/.../a2a (JSON-RPC request → SSE JSON-RPC stream).
///
/// Parent: when the inbound request carries W3C `traceparent`, the caller
/// attaches that context via `OpenTelemetrySpanExt::set_parent(..)` before
/// entering the span so forwarded A2A requests appear as one distributed
/// trace across ingress + serving runners. `otel.kind = "server"` pairs with
/// the ingress `baml_rt.cluster_a2a_forward` client span.
///
/// `forwarded` is derived from inbound `baggage: ingress_service_instance_id=…`
/// (advisory on public routes — see `otel_middleware` module doc for the
/// spoofability disclosure). `ingress_service_instance_id` tracks the runner
/// that accepted the request (local pod when absent from baggage);
/// `serving_service_instance_id` is always the local runner.
/// `target_service_instance_id` stays `None` at this layer; the ingress-side
/// `cluster_a2a_forward` span carries the resolved target identity.
///
/// Takes typed identifiers so raw path input cannot reach this info-level span —
/// callers must parse first.
#[inline]
pub(crate) fn post_a2a(
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    forwarded: bool,
    ingress_service_instance_id: &str,
    serving_service_instance_id: &str,
    target_service_instance_id: Option<&str>,
) -> Span {
    tracing::info_span!(
        "baml_rt_api.post_a2a",
        otel.kind = "server",
        agent_package = %agent_package,
        agent_instance_id = %agent_instance_id,
        forwarded = forwarded,
        ingress_service_instance_id = %ingress_service_instance_id,
        serving_service_instance_id = %serving_service_instance_id,
        target_service_instance_id = target_service_instance_id.unwrap_or(UNKNOWN_SERVICE_INSTANCE_ID),
    )
}

/// Create span for POST /agents/.../dispatch (deterministic buffered delivery).
///
/// Parent: HTTP request span. Ingress milestone — see [`post_a2a`] for field semantics
/// and the typed-identifier requirement.
#[inline]
pub(crate) fn post_dispatch(
    agent_package: &AgentPackageName,
    agent_instance_id: &AgentInstanceId,
    forwarded: bool,
    ingress_service_instance_id: &str,
    serving_service_instance_id: &str,
    target_service_instance_id: Option<&str>,
) -> Span {
    tracing::info_span!(
        "baml_rt_api.post_dispatch",
        otel.kind = "server",
        agent_package = %agent_package,
        agent_instance_id = %agent_instance_id,
        forwarded = forwarded,
        ingress_service_instance_id = %ingress_service_instance_id,
        serving_service_instance_id = %serving_service_instance_id,
        target_service_instance_id = target_service_instance_id.unwrap_or(UNKNOWN_SERVICE_INSTANCE_ID),
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
pub(crate) fn get_context_index() -> Span {
    tracing::debug_span!("baml_rt_api.get_context_index")
}

#[inline]
pub(crate) fn get_provenance_aggregates() -> Span {
    tracing::debug_span!("baml_rt_api.get_provenance_aggregates")
}

#[inline]
pub(crate) fn get_provenance_lifecycle_events() -> Span {
    tracing::debug_span!("baml_rt_api.get_provenance_lifecycle_events")
}

#[inline]
pub(crate) fn get_episode(task_id: &str) -> Span {
    tracing::debug_span!("baml_rt_api.episode.get", task_id = %task_id)
}

#[inline]
pub(crate) fn get_conversation_history(context_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_api.conversation_history.get",
        context_id = %context_id
    )
}

#[inline]
pub(crate) fn get_conversation_history_stream(context_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_api.conversation_history.stream",
        context_id = %context_id
    )
}
