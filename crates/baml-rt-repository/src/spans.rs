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

/// Span for retrieving an entry by content hash.
///
/// Parent: HTTP request span.
/// Children: metadata read, blob read.
#[inline]
pub(crate) fn get_by_hash(hash: &str) -> Span {
    tracing::debug_span!("repository.get_by_hash", hash = hash,)
}

/// Span for retrieving an entry by name + version.
///
/// Parent: HTTP request span.
/// Children: metadata read.
#[inline]
pub(crate) fn get_by_version(name: &str, version: &str) -> Span {
    tracing::debug_span!(
        "repository.get_by_version",
        agent_name = name,
        version = version,
    )
}

/// Span for lineage subgraph query.
///
/// Parent: HTTP request span.
/// Children: lineage store traversal.
#[inline]
pub(crate) fn lineage_query(hash: &str, depth: u32) -> Span {
    tracing::debug_span!("repository.lineage_query", hash = hash, depth = depth,)
}

/// Span for search execution.
///
/// Parent: HTTP request span.
/// Children: search store query.
#[inline]
pub(crate) fn search() -> Span {
    tracing::debug_span!("repository.search")
}

/// Span for top-by-fitness query (ADAS hot path).
///
/// Parent: HTTP request span.
/// Children: search store query.
#[inline]
pub(crate) fn top_by_fitness(domain: &str, limit: usize) -> Span {
    tracing::debug_span!("repository.top_by_fitness", domain = domain, limit = limit,)
}

/// Span for recording a fitness score.
///
/// Parent: HTTP request span.
/// Children: metadata write.
#[inline]
pub(crate) fn record_fitness(hash: &str, domain: &str) -> Span {
    tracing::debug_span!("repository.record_fitness", hash = hash, domain = domain,)
}

/// Span for canonical hash computation.
///
/// Parent: publish or fork span.
#[inline]
pub(crate) fn compute_hash() -> Span {
    tracing::debug_span!("repository.compute_hash")
}

/// Span for blob store write.
///
/// Parent: publish or fork span.
#[inline]
pub(crate) fn blob_write(hash: &str) -> Span {
    tracing::debug_span!("repository.blob_write", hash = hash,)
}

/// Span for blob store read.
///
/// Parent: get_by_hash span.
#[inline]
pub(crate) fn blob_read(hash: &str) -> Span {
    tracing::debug_span!("repository.blob_read", hash = hash,)
}
