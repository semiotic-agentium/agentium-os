//! OpenTelemetry span helpers for the agent repository.
//!
//! Orthogonal to business logic. All span names are static with the
//! `repository.{operation}` namespace. Dynamic data is in structured fields.

use tracing::Span;

/// Span for publishing a new agent version.
///
/// Parent: HTTP request span (auto-attached).
/// Children: storage write, hash computation, lineage edge recording.
#[inline]
pub(crate) fn publish(agent_name: &str) -> Span {
    tracing::info_span!("repository.publish", agent_name = agent_name,)
}

/// Span for forking an agent into a new lineage.
///
/// Parent: HTTP request span.
/// Children: storage write, lineage edge recording.
#[inline]
pub(crate) fn fork(source_hash: &str, new_name: &str) -> Span {
    tracing::info_span!(
        "repository.fork",
        source_hash = source_hash,
        new_name = new_name,
    )
}
