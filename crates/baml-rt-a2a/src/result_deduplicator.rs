use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};

use async_trait::async_trait;
use baml_rt_core::Result;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::result_pipeline::ResultStoragePipeline;

#[async_trait]
pub trait ResultDeduplicator: Send + Sync {
    async fn should_process(&self, value: &Value) -> bool;
    async fn mark_processed(&self, value: &Value);
}

pub struct HashResultDeduplicator {
    seen: Mutex<HashSet<u64>>,
}

impl HashResultDeduplicator {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
        }
    }

    fn hash_value(value: &Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        let serialized = serde_json::to_string(value).unwrap_or_default();
        serialized.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for HashResultDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ResultDeduplicator for HashResultDeduplicator {
    async fn should_process(&self, value: &Value) -> bool {
        let hash = Self::hash_value(value);
        let seen = self.seen.lock().await;
        !seen.contains(&hash)
    }

    async fn mark_processed(&self, value: &Value) {
        let hash = Self::hash_value(value);
        let mut seen = self.seen.lock().await;
        seen.insert(hash);
    }
}

pub struct DeduplicatingPipeline {
    inner: Arc<dyn ResultStoragePipeline>,
    deduplicator: Arc<dyn ResultDeduplicator>,
}

impl DeduplicatingPipeline {
    pub fn new(
        inner: Arc<dyn ResultStoragePipeline>,
        deduplicator: Arc<dyn ResultDeduplicator>,
    ) -> Self {
        Self {
            inner,
            deduplicator,
        }
    }
}

#[async_trait]
impl ResultStoragePipeline for DeduplicatingPipeline {
    async fn store_result(&self, value: &Value) -> Result<()> {
        if self.deduplicator.should_process(value).await {
            self.inner.store_result(value).await?;
            self.deduplicator.mark_processed(value).await;
        }
        Ok(())
    }
}
