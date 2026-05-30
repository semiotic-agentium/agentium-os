// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host contracts for durable ingress inbox persistence.
//!
//! # Architecture (proportional layering)
//!
//! Dependencies flow **inward** to this crate; nothing here depends on tool bundles.
//!
//! | Concern | Crate / location |
//! | --- | --- |
//! | [`IngressStore`], [`IngressItem`], [`IngressId`] | **`baml-rt-core`** (this module) |
//! | `install_ingress_store` / `require_ingress_store` globals | **`baml-rt-tools`** (`ingress_store.rs`) — service locator |
//! | Surreal persistence implementing [`IngressStore`] | **`baml-agent-runner`** (`deployment_state::DeploymentStateStore`) |
//!
//! The runner binary wires the store at startup by calling
//! `baml_rt_tools::ingress_store::install_ingress_store` with an `Arc<dyn IngressStore>`
//! backed by `DeploymentStateStore`.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{BamlRtError, Result, event_subscription::EventSourceKey};

/// Opaque, content-derived identifier for a single ingress batch.
///
/// Producers generate deterministic IDs (e.g. from source key + message hashes)
/// so the store can deduplicate re-polls of the same data.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct IngressId(String);

impl IngressId {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        let trimmed = value.as_ref().trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IngressId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for IngressId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).ok_or_else(|| serde::de::Error::custom("invalid ingress id"))
    }
}

/// A single ingress record ready for durable storage.
///
/// The `payload_json` field is an opaque JSON string owned by the producing tool.
/// The store persists it as-is; only the tool knows how to interpret it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngressItem {
    pub ingress_id: IngressId,
    pub source_key: EventSourceKey,
    pub payload_json: String,
    pub enqueued_at_unix_ms: u64,
}

/// Durable store for ingress items from any event source.
///
/// State machine: **pending** -> **emitted** -> **delivered**.
///
/// - `enqueue`: inserts a new pending item (idempotent by ingress_id)
/// - `list_pending`: returns items that are pending and not yet emitted
/// - `mark_emitted`: claims pending items for emission (returns only successfully claimed IDs)
/// - `mark_delivered`: confirms delivery of emitted items
/// - `requeue_stale`: returns emitted-but-unconfirmed items to pending after a timeout
///
/// Delivery is intentionally at-least-once. Consuming agents should treat payloads as idempotent.
#[async_trait]
pub trait IngressStore: Send + Sync {
    async fn enqueue(&self, item: &IngressItem) -> Result<bool>;

    async fn list_pending(&self, limit: usize) -> Result<Vec<IngressItem>>;

    async fn requeue_stale(&self, emitted_before_unix_ms: u64) -> Result<usize>;

    async fn mark_emitted(
        &self,
        ingress_ids: &[IngressId],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<IngressId>>;

    async fn mark_delivered(
        &self,
        ingress_ids: &[IngressId],
        delivered_at_unix_ms: u64,
    ) -> Result<()>;
}

/// Used by `baml-rt-tools` when the global ingress store is missing.
pub fn ingress_store_not_installed() -> BamlRtError {
    BamlRtError::InvalidArgument("ingress store is not installed in this host".to_string())
}

#[cfg(test)]
mod tests {
    use super::IngressId;

    #[test]
    fn ingress_id_deserialize_rejects_blank_values() {
        let err = serde_json::from_str::<IngressId>("\"   \"")
            .expect_err("blank ingress ids should be rejected");
        assert!(err.to_string().contains("invalid ingress id"));
    }
}
