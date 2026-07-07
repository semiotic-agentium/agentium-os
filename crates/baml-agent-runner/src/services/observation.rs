// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Unified operator observation bundle (planning + ops) via [`ObservationLoader`].

use std::sync::Arc;

use baml_rt_api::{
    ContextPlanningResponse, ObservationBundleDto, ObservationError, ObservationRequest,
    ObservationService, TaskPlanningSnapshot, summarize_plan_steps,
};
use baml_rt_provenance::{
    LoadedPlanningSlice, ObservationBundleRequest, ObservationLoader as _,
    surreal_store::SurrealProvenanceStore,
};

pub(crate) struct ObservationServiceImpl {
    store: Arc<SurrealProvenanceStore>,
}

impl ObservationServiceImpl {
    pub(crate) fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl ObservationService for ObservationServiceImpl {
    async fn bundle(
        &self,
        request: ObservationRequest,
    ) -> Result<ObservationBundleDto, ObservationError> {
        let include = request.include;
        let loaded = self
            .store
            .bundle(observation_bundle_request_from_api(&request))
            .await
            .map_err(|e| {
                baml_rt_api::service_error::ServiceError::Other(Box::new(std::io::Error::other(e)))
            })?;

        let planning = if include.planning || include.drift || include.gate {
            match loaded.planning {
                Some(slice) => Some(
                    planning_response_from_slice(
                        &self.store,
                        loaded.context_id.as_str(),
                        slice,
                        include.drift,
                        include.gate,
                    )
                    .await
                    .map_err(|e| {
                        baml_rt_api::service_error::ServiceError::Other(Box::new(
                            std::io::Error::other(e),
                        ))
                    })?,
                ),
                None => None,
            }
        } else {
            None
        };

        Ok(ObservationBundleDto {
            context_id: loaded.context_id.as_str().to_string(),
            version: loaded.version.as_str().to_string(),
            planning,
            llm_ops: loaded.llm_ops,
            tool_ops: loaded.tool_ops,
        })
    }
}

fn observation_bundle_request_from_api(request: &ObservationRequest) -> ObservationBundleRequest {
    ObservationBundleRequest {
        context_id: request.context_id.clone(),
        task_id: request.task_id.clone(),
        agent_package: request.agent_package.clone(),
        agent_id: request.agent_id.clone(),
        include_planning: request.include.planning,
        include_drift: request.include.drift,
        include_gate: request.include.gate,
        include_llm_ops: request.include.llm_ops,
        include_tool_ops: request.include.tool_ops,
        ops_page_size: request.ops_page_size,
        planning_history_limit: request.planning_history_limit,
    }
}

async fn planning_response_from_slice(
    store: &SurrealProvenanceStore,
    context_id: &str,
    slice: LoadedPlanningSlice,
    include_drift: bool,
    include_gate: bool,
) -> Result<ContextPlanningResponse, baml_rt_provenance::ProvenanceError> {
    let mut tasks = Vec::with_capacity(slice.tasks.len());
    for row in slice.tasks {
        let drift = if include_drift {
            super::planning::PlanningServiceImpl::aggregate_drift(store, context_id, &row.task_id)
                .await
        } else {
            None
        };
        let gate = if include_gate {
            super::planning::PlanningServiceImpl::aggregate_gate(store, context_id, &row.task_id)
                .await
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
        context_id: context_id.to_string(),
        all_task_ids: slice.all_task_ids,
        tasks,
    })
}
