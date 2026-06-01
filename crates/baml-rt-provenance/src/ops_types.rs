// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed provenance ops query response shapes (operator / API wire contract).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One ops query row — canonical scalar accessors plus extension map for nested payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProvenanceOpsRow(pub Map<String, Value>);

impl ProvenanceOpsRow {
    #[must_use]
    pub fn from_map(map: Map<String, Value>) -> Self {
        Self(map)
    }

    #[must_use]
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }

    #[must_use]
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    #[must_use]
    pub fn activity_id(&self) -> Option<&str> {
        self.get("activity_id")?.as_str()
    }

    #[must_use]
    pub fn context_id(&self) -> Option<&str> {
        self.get("context_id")?.as_str()
    }

    #[must_use]
    pub fn task_id(&self) -> Option<&str> {
        self.get("task_id")?.as_str()
    }

    #[must_use]
    pub fn timestamp_ms(&self) -> Option<u64> {
        self.get("timestamp_ms")?.as_u64()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsPercentileHotspots {
    pub p95: f64,
    pub p99: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceOpsSummary {
    pub count: u64,
    pub failed_count: u64,
    pub duration_ms_total: u64,
    pub total_tokens: u64,
    pub prompt_tokens_total: u64,
    pub completion_tokens_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens_total: Option<u64>,
    pub latency_hotspots: OpsPercentileHotspots,
    pub token_hotspots: OpsPercentileHotspots,
}

impl ProvenanceOpsSummary {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            count: 0,
            failed_count: 0,
            duration_ms_total: 0,
            total_tokens: 0,
            prompt_tokens_total: 0,
            completion_tokens_total: 0,
            cached_input_tokens_total: None,
            latency_hotspots: OpsPercentileHotspots { p95: 0.0, p99: 0.0 },
            token_hotspots: OpsPercentileHotspots { p95: 0.0, p99: 0.0 },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceOpsHotspotGroup {
    pub group_key: String,
    pub group_values: Vec<Option<String>>,
    pub group_dimensions: Vec<String>,
    pub count: u64,
    pub failed: u64,
    pub failure_rate: f64,
    pub avg_duration_ms: f64,
    pub avg_total_tokens: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceOpsAppliedCaps {
    pub page_size: u32,
    pub max_page_size: u32,
    pub top_k: u32,
}
