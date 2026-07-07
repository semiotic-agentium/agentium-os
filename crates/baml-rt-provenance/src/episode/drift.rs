// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Citation integrity aggregation for episode assembly.

use baml_rt_core::ids::{ContextId, TaskId};

use crate::{
    error::Result,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    },
};

/// Query and aggregate per-call citation integrity for a task.
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
            response_profile: Some(crate::store::ProvenanceResponseProfile::ToolCompact),
            page_size: Some(500),
            sort_by: Some("timestamp_ms".to_string()),
            sort_dir: Some("desc".to_string()),
            ..Default::default()
        })
        .await?;

    let mut scored_count = 0u32;
    let mut warn_count = 0u32;
    let mut drift_calls = Vec::new();
    let mut worst_unresolved = 0u32;

    for row in &report.rows {
        let row_obj = row.as_map();
        let Some(drift_obj) = row_obj.get("drift") else {
            continue;
        };
        let Some(citation) = drift_obj.get("citation") else {
            continue;
        };
        scored_count += 1;

        let unresolved = citation
            .get("unresolvedCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let resolved = citation
            .get("resolvedCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if unresolved > worst_unresolved {
            worst_unresolved = unresolved;
        }
        if unresolved > 0 {
            warn_count += 1;
        }

        let activity_anchor = row_obj
            .get("a2a_activity_anchor")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let cite_strings: Vec<String> = citation
            .get("perCitation")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.get("raw").and_then(|r| r.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        drift_calls.push(super::EpisodeDriftCall {
            activity_anchor,
            function_name: row_obj
                .get("baml_prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            severity: if unresolved > 0 {
                "warn".to_string()
            } else {
                "acceptable".to_string()
            },
            intent_alignment: 0.0,
            step_alignment: None,
            cross_encoder_step_score: None,
            trajectory_drift: None,
            plan_adherence_score: 0.0,
            citation_mean_similarity: None,
            citation_coverage: if resolved + unresolved > 0 {
                Some(resolved as f32 / (resolved + unresolved) as f32)
            } else {
                None
            },
            citation_strings: cite_strings,
        });
    }

    let drift_summary = if scored_count > 0 {
        Some(super::EpisodeDriftSummary {
            composite_severity: if worst_unresolved > 0 {
                "warn".to_string()
            } else {
                "acceptable".to_string()
            },
            intent_alignment: 0.0,
            step_alignment: None,
            trajectory_drift: None,
            plan_adherence_score: 0.0,
            scored_call_count: scored_count,
            warn_count,
            block_count: 0,
        })
    } else {
        None
    };

    Ok((drift_summary, drift_calls))
}
