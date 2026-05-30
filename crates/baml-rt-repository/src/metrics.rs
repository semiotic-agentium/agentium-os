// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry metrics helpers for the agent repository.
//!
//! Orthogonal to business logic. All metric names are static with the
//! `repository.{domain}.{metric_type}` namespace. Instruments are cached
//! in `OnceLock` for zero-allocation recording on the hot path.

use std::{sync::OnceLock, time::Duration};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

// ---------------------------------------------------------------------------
// Cached instruments
// ---------------------------------------------------------------------------

static PUBLISH_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static PUBLISH_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
static FORK_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static SEARCH_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static SEARCH_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
static BLOB_READ_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static BLOB_WRITE_COUNTER: OnceLock<Counter<u64>> = OnceLock::new();
static HASH_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();

fn publish_counter() -> &'static Counter<u64> {
    PUBLISH_COUNTER.get_or_init(|| {
        global::meter("baml_rt_repository")
            .u64_counter("repository.publish.total")
            .init()
    })
}

fn publish_duration() -> &'static Histogram<f64> {
    PUBLISH_DURATION.get_or_init(|| {
        global::meter("baml_rt_repository")
            .f64_histogram("repository.publish.duration_ms")
            .init()
    })
}

fn fork_counter() -> &'static Counter<u64> {
    FORK_COUNTER.get_or_init(|| {
        global::meter("baml_rt_repository")
            .u64_counter("repository.fork.total")
            .init()
    })
}

fn search_counter() -> &'static Counter<u64> {
    SEARCH_COUNTER.get_or_init(|| {
        global::meter("baml_rt_repository")
            .u64_counter("repository.search.total")
            .init()
    })
}

fn search_duration() -> &'static Histogram<f64> {
    SEARCH_DURATION.get_or_init(|| {
        global::meter("baml_rt_repository")
            .f64_histogram("repository.search.duration_ms")
            .init()
    })
}

fn blob_read_counter() -> &'static Counter<u64> {
    BLOB_READ_COUNTER.get_or_init(|| {
        global::meter("baml_rt_repository")
            .u64_counter("repository.blob.read_total")
            .init()
    })
}

fn blob_write_counter() -> &'static Counter<u64> {
    BLOB_WRITE_COUNTER.get_or_init(|| {
        global::meter("baml_rt_repository")
            .u64_counter("repository.blob.write_total")
            .init()
    })
}

fn hash_duration() -> &'static Histogram<f64> {
    HASH_DURATION.get_or_init(|| {
        global::meter("baml_rt_repository")
            .f64_histogram("repository.hash.duration_ms")
            .init()
    })
}

// ---------------------------------------------------------------------------
// Public recording functions
// ---------------------------------------------------------------------------

/// Record a publish operation (success or failure).
pub(crate) fn record_publish(agent_name: &str, result: &str, duration: Duration) {
    let attrs = &[
        KeyValue::new("agent_name", agent_name.to_string()),
        KeyValue::new("result", result.to_string()),
    ];
    publish_counter().add(1, attrs);
    publish_duration().record(duration.as_millis() as f64, attrs);
}

/// Record a fork operation.
pub(crate) fn record_fork(source_name: &str, new_name: &str, result: &str) {
    fork_counter().add(
        1,
        &[
            KeyValue::new("source_name", source_name.to_string()),
            KeyValue::new("new_name", new_name.to_string()),
            KeyValue::new("result", result.to_string()),
        ],
    );
}

/// Record a search operation.
pub(crate) fn record_search(result_count: usize, duration: Duration) {
    let attrs = &[KeyValue::new(
        "result_count_bucket",
        bucket_count(result_count),
    )];
    search_counter().add(1, attrs);
    search_duration().record(duration.as_millis() as f64, attrs);
}

/// Record a blob read.
pub(crate) fn record_blob_read(result: &str) {
    blob_read_counter().add(1, &[KeyValue::new("result", result.to_string())]);
}

/// Record a blob write.
pub(crate) fn record_blob_write(result: &str) {
    blob_write_counter().add(1, &[KeyValue::new("result", result.to_string())]);
}

/// Record hash computation duration.
pub(crate) fn record_hash_duration(duration: Duration) {
    hash_duration().record(duration.as_millis() as f64, &[]);
}

/// Low-cardinality bucket for result counts.
fn bucket_count(count: usize) -> &'static str {
    match count {
        0 => "0",
        1..=5 => "1-5",
        6..=20 => "6-20",
        21..=100 => "21-100",
        _ => "100+",
    }
}
