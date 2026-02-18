//! OpenTelemetry span helpers for provenance Cypher execution.
//!
//! Follows the OTel instrumentation guide: static span names, structured fields.
//! Caller should enter the span and then emit a debug log with query_text and
//! params as separate fields so params are never interpolated into the query string.

use serde_json::Value;
use tracing::Span;

/// Create span for a single Cypher execution (read or write).
///
/// Attributes: db.operation, db.query.text, cypher.params (JSON string).
/// Caller must enter the span and log at debug with query_text and params separate.
#[inline]
pub(crate) fn cypher_execute(query: &str, params: &Value) -> Span {
    let params_str = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());
    tracing::debug_span!(
        "baml_rt_provenance.cypher_execute",
        db.operation = "cypher",
        db.query.text = query,
        cypher.params = %params_str,
    )
}
