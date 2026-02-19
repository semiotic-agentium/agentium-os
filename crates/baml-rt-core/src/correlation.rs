//! Correlation ID propagation for async invocation flows.
//!
//! This module provides task-local correlation IDs so async boundaries
//! can retain request context without requiring JS changes.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::ids::CorrelationId;

tokio::task_local! {
    static CORRELATION_ID: CorrelationId;
}

static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn generate_correlation_id() -> CorrelationId {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let counter = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    CorrelationId::new(millis, counter)
}

pub fn current_correlation_id() -> Option<CorrelationId> {
    CORRELATION_ID.try_with(|id| id.clone()).ok()
}

pub fn current_or_new() -> CorrelationId {
    current_correlation_id().unwrap_or_else(generate_correlation_id)
}

pub async fn with_correlation_id<F, T>(id: CorrelationId, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    CORRELATION_ID.scope(id, fut).await
}
