// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Ops projection helpers — align summary counts with episode TASK_CALL semantics.

use crate::{ops_types::ProvenanceOpsSummary, store::ProvenanceOpsQueryResponse};

/// Override LLM summary `count` with task-scoped TASK_CALL aggregate (matches episode).
pub fn project_ops_llm_summary_count(response: &mut ProvenanceOpsQueryResponse, count: u32) {
    response.summary.count = u64::from(count);
}

/// Build summary from aggregated row metrics.
#[must_use]
pub fn build_ops_summary(
    rows: &[serde_json::Map<String, serde_json::Value>],
    include_cached_tokens: bool,
    duration_p95: f64,
    duration_p99: f64,
    token_p95: f64,
    token_p99: f64,
) -> ProvenanceOpsSummary {
    use crate::ops_types::OpsPercentileHotspots;

    let failed_count = rows.iter().filter(|r| ops_row_failed(r)).count() as u64;
    let total_tokens_sum: u64 = rows
        .iter()
        .map(|r| r.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();
    let prompt_tokens_sum: u64 = rows
        .iter()
        .map(|r| r.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();
    let completion_tokens_sum: u64 = rows
        .iter()
        .map(|r| {
            r.get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        })
        .sum();
    let cached_input_tokens_sum: u64 = rows
        .iter()
        .map(|r| {
            r.get("cached_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        })
        .sum();
    let total_duration_sum: u64 = rows
        .iter()
        .map(|r| r.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();

    ProvenanceOpsSummary {
        count: rows.len() as u64,
        failed_count,
        duration_ms_total: total_duration_sum,
        total_tokens: total_tokens_sum,
        prompt_tokens_total: prompt_tokens_sum,
        completion_tokens_total: completion_tokens_sum,
        cached_input_tokens_total: include_cached_tokens.then_some(cached_input_tokens_sum),
        latency_hotspots: OpsPercentileHotspots {
            p95: duration_p95,
            p99: duration_p99,
        },
        token_hotspots: OpsPercentileHotspots {
            p95: token_p95,
            p99: token_p99,
        },
    }
}

fn ops_row_failed(row: &serde_json::Map<String, serde_json::Value>) -> bool {
    row.get("activity_outcome")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("failed"))
}
