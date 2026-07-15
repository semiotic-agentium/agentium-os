// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Planning and intent/plan history service — batched graph reads.

use std::sync::Arc;

use baml_rt_api::{
    ContextPlanningResponse, GateEventDetail, PlanningScopeRequest, PlanningService,
    TaskGateSummary, TaskPlanDriftSummary, TaskPlanningSnapshot, summarize_plan_steps,
};
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use baml_rt_provenance::{
    episode::{aggregate_task_drift, aggregate_task_gate},
    surreal_store::PlanningScopeQuery,
};

pub(crate) struct PlanningServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl PlanningServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }

    pub(super) async fn aggregate_gate(
        store: &baml_rt_provenance::SurrealProvenanceStore,
        context_id: &str,
        task_id: &str,
    ) -> Option<TaskGateSummary> {
        let ctx_id = ContextId::from(context_id);
        let tid = TaskId::from_external(ExternalId::new(task_id.to_string()));
        let agg = aggregate_task_gate(store, &ctx_id, &tid).await.ok()??;
        let gate_events = agg
            .gate_events
            .into_iter()
            .map(|e| GateEventDetail {
                tool_name: e.tool_name,
                tier: e.tier,
                decision: e.decision,
                reason_code: e.reason_code,
                deficient_nodes: e.deficient_nodes,
                tool_call_anchor: e.tool_call_anchor,
            })
            .collect();
        let prevented = agg.prevented_error_count;
        let friction = agg.friction_denial_count;
        let prevention_ratio = if prevented + friction > 0 {
            Some(prevented as f32 / (prevented + friction) as f32)
        } else {
            None
        };
        Some(TaskGateSummary {
            deny_count: agg.deny_count,
            ask_count: agg.ask_count,
            pass_gated_count: agg.pass_gated_count,
            pass_count: agg.pass_count,
            prevented_error_count: prevented,
            friction_denial_count: friction,
            prevention_ratio,
            gate_events,
        })
    }

    pub(super) async fn aggregate_drift(
        store: &baml_rt_provenance::SurrealProvenanceStore,
        context_id: &str,
        task_id: &str,
    ) -> Option<baml_rt_api::TaskPlanDriftSummary> {
        let ctx_id = ContextId::from(context_id);
        let tid = TaskId::from_external(ExternalId::new(task_id.to_string()));
        let (drift_summary, drift_calls) = aggregate_task_drift(store, &ctx_id, &tid).await.ok()?;
        let summary = drift_summary?;

        let mut drifted_calls = Vec::new();
        for call in drift_calls {
            if call.severity == "warn" {
                drifted_calls.push(baml_rt_api::DriftedCallDetail {
                    function_name: call.function_name,
                    severity: call.severity.as_str().to_owned(),
                    intent_alignment: call.intent_alignment,
                    step_alignment: call.step_alignment,
                    cross_encoder_step_score: call.cross_encoder_step_score,
                    intent_text_preview: String::new(),
                    response_text_preview: String::new(),
                    step_text_preview: String::new(),
                    citations: Vec::new(),
                });
            }
        }

        Some(TaskPlanDriftSummary {
            composite_severity: Some(summary.composite_severity.as_str().to_owned()),
            intent_alignment: Some(summary.intent_alignment),
            step_alignment: summary.step_alignment,
            trajectory_drift: summary.trajectory_drift,
            plan_adherence_score: Some(summary.plan_adherence_score),
            scored_call_count: summary.scored_call_count,
            warn_count: summary.warn_count,
            block_count: summary.block_count,
            drifted_calls,
        })
    }
}

#[async_trait::async_trait]
impl PlanningService for PlanningServiceImpl {
    async fn planning_for_scope(
        &self,
        request: PlanningScopeRequest,
    ) -> Result<ContextPlanningResponse, baml_rt_api::PlanningError> {
        let context_id = ContextId::from(request.context_id.as_str());
        let scope = PlanningScopeQuery::from(&request);

        let (all_task_ids, batch_rows) =
            self.store.query_planning_batch(&scope).await.map_err(|e| {
                baml_rt_api::PlanningError::Other(Box::new(std::io::Error::other(e)))
            })?;

        let mut tasks = Vec::with_capacity(batch_rows.len());
        for row in batch_rows {
            let drift = if request.include_drift {
                Self::aggregate_drift(&self.store, context_id.as_str(), &row.task_id).await
            } else {
                None
            };
            let gate = if request.include_gate {
                Self::aggregate_gate(&self.store, context_id.as_str(), &row.task_id).await
            } else {
                None
            };
            let step_summary = summarize_plan_steps(row.current_plan.as_ref());
            tasks.push(TaskPlanningSnapshot {
                task_id: row.task_id,
                current_intent: row.current_intent,
                current_plan: row.current_plan,
                intent_history: row.intent_history,
                plan_history: row.plan_history,
                step_summary,
                gate,
                drift,
            });
        }

        Ok(ContextPlanningResponse {
            context_id: context_id.as_str().to_string(),
            all_task_ids,
            tasks,
        })
    }
}
