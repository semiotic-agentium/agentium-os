//! OpenTelemetry span helpers for provenance.
//!
//! Follows the OTel instrumentation guide: static span names, structured fields.
//! Caller should enter the span and then emit a debug log with query_text and
//! params as separate fields so params are never interpolated into the query string.

use tracing::Span;

/// Create span for exporting the provenance graph scoped to a `context_id`.
///
/// Parent: caller (e.g. HTTP handler). Children: SurrealQL work inside `GraphExporter`.
#[inline]
pub(crate) fn graph_export_by_context(context_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_provenance.graph_export.by_context",
        provenance.context_id = context_id,
    )
}

/// Create span for exporting the provenance graph scoped to a `task_id`.
///
/// Parent: caller. Resolves context then reuses context export path.
#[inline]
pub(crate) fn graph_export_by_task(task_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_provenance.graph_export.by_task",
        provenance.task_id = task_id,
    )
}

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
