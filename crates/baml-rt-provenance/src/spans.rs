//! OpenTelemetry span helpers for provenance.
//!
//! Follows the OTel instrumentation guide: static span names, structured fields.
//! Caller should enter the span and then emit a debug log with query_text and
//! params as separate fields so params are never interpolated into the query string.

use serde_json::Value;
use tracing::Span;

/// Create span for a single SurrealQL execution (read or write).
///
/// Create span for sequence diagram rendering (graph → Mermaid string).
///
/// Level: debug — business operation (graph transform), not HTTP or low-level protocol.
/// Parent: typically export_by_context or mermaid cache lookup.
#[inline]
pub(crate) fn sequence_render(nodes_count: usize, edges_count: usize, scope: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_provenance.sequence.render",
        provenance.sequence.nodes_count = nodes_count,
        provenance.sequence.edges_count = edges_count,
        provenance.sequence.scope = scope,
    )
}
