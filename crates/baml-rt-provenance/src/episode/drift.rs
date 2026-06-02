// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Drift aggregation for episode assembly.

use baml_rt_core::ids::{ContextId, TaskId};
use baml_rt_embedding::DriftSeverity;

use crate::{
    error::Result,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    },
};

/// Query and aggregate per-call drift data for a task into episode-appropriate structs.
///
/// Accepts any `ProvenanceOpsQuery` implementation so the aggregation logic is decoupled from
/// the concrete `SurrealProvenanceStore` and can be tested with a mock store.
pub async fn aggregate_task_drift(
    store: &dyn ProvenanceOpsQuery,
    context_id: &ContextId,
    task_id: &TaskId,
) -> Result<(
    Option<super::EpisodeDriftSummary>,
    Vec<super::EpisodeDriftCall>,
)> {
    let report = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::LlmCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
                task_id: Some(task_id.clone()),
                ..Default::default()
            },
            // Drift scoring only inspects inline node properties — full prompt/result
            // payloads are never read here, so use the compact profile to avoid loading them.
            response_profile: Some(crate::store::ProvenanceResponseProfile::ToolCompact),
            // 500 gives ample headroom for long tasks without over-fetching.
            page_size: Some(500),
            sort_by: Some("timestamp_ms".to_string()),
            sort_dir: Some("desc".to_string()),
            ..Default::default()
        })
        .await?;

    let mut scored_count = 0u32;
    let mut warn_count = 0u32;
    let mut block_count = 0u32;
    // Track the call with the worst composite severity (not just the most recent call)
    // so the summary headline reflects the most concerning event in the task.
    let mut worst_plan_drift: Option<&serde_json::Value> = None;
    let mut drift_calls = Vec::new();

    fn f32_field(obj: &serde_json::Value, key: &str) -> Option<f32> {
        obj.get(key).and_then(|v| v.as_f64()).map(|v| v as f32)
    }

    fn sev_field(obj: &serde_json::Value, key: &str) -> DriftSeverity {
        obj.get(key)
            .and_then(|v| v.as_str())
            .map(DriftSeverity::from_wire_str)
            .unwrap_or(DriftSeverity::Acceptable)
    }

    for row in &report.rows {
        let row_obj = row.as_map();
        let Some(drift_obj) = row_obj.get("drift") else {
            continue;
        };
        let Some(plan) = drift_obj.get("plan") else {
            continue;
        };
        scored_count += 1;

        let call_sev = sev_field(plan, "compositeSeverity");
        let worst_sev = worst_plan_drift
            .map(|p| sev_field(p, "compositeSeverity"))
            .unwrap_or(DriftSeverity::Acceptable);
        if worst_plan_drift.is_none() || call_sev > worst_sev {
            worst_plan_drift = Some(plan);
        }

        let activity_anchor = row_obj
            .get("a2a_activity_anchor")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match call_sev {
            DriftSeverity::Warn => warn_count += 1,
            DriftSeverity::Block => block_count += 1,
            DriftSeverity::Acceptable => {}
        }

        // Citation arrives as a parsed Object — `nest_llm_drift_fields` in
        // `surreal_store.rs` deserialises any `Value::String` form at query time.
        let (cite_mean, cite_cov, cite_strings) = match drift_obj.get("citation") {
            Some(serde_json::Value::Object(obj)) => {
                let mean = obj
                    .get("meanSimilarity")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32);
                let cov = obj
                    .get("coverage")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32);
                let strings: Vec<String> = obj
                    .get("perCitation")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| c.get("raw").and_then(|r| r.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                (mean, cov, strings)
            }
            _ => (None, None, Vec::new()),
        };

        drift_calls.push(super::EpisodeDriftCall {
            activity_anchor,
            function_name: row_obj
                .get("baml_prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            severity: call_sev,
            intent_alignment: f32_field(plan, "intentAlignment").unwrap_or(0.0),
            step_alignment: f32_field(plan, "stepAlignment"),
            cross_encoder_step_score: f32_field(plan, "crossEncoderStepScore"),
            trajectory_drift: f32_field(plan, "trajectoryDrift"),
            plan_adherence_score: f32_field(plan, "planAdherenceScore").unwrap_or(0.0),
            citation_mean_similarity: cite_mean,
            citation_coverage: cite_cov,
            citation_strings: cite_strings,
        });
    }

    let drift_summary = worst_plan_drift.map(|plan| super::EpisodeDriftSummary {
        composite_severity: sev_field(plan, "compositeSeverity"),
        intent_alignment: f32_field(plan, "intentAlignment").unwrap_or(0.0),
        step_alignment: f32_field(plan, "stepAlignment"),
        trajectory_drift: f32_field(plan, "trajectoryDrift"),
        plan_adherence_score: f32_field(plan, "planAdherenceScore").unwrap_or(0.0),
        scored_call_count: scored_count,
        warn_count,
        block_count,
    });

    Ok((drift_summary, drift_calls))
}
