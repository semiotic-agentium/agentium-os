//! Unified operator observation bundle — planning + ops via one loader path.

use super::{
    engine::ObservationLoader,
    fingerprint::observation_version_from_bundle,
    types::{LoadedObservationBundle, LoadedPlanningSlice, ObservationBundleRequest, OpsQueryMode},
};
use crate::{
    error::Result,
    observation::scope::observation_scope_from_ops_filters,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQueryRequest, ProvenanceOpsQueryResponse,
        ProvenanceOpsResource, ProvenanceOutcomeSegment, ProvenanceResponseProfile,
    },
    surreal_store::{PlanningScopeQuery, SurrealProvenanceStore},
};

impl SurrealProvenanceStore {
    /// Load planning + ops slices for operator `/observe` (index-authoritative planning).
    pub async fn load_observation_bundle(
        &self,
        request: ObservationBundleRequest,
    ) -> Result<LoadedObservationBundle> {
        let planning = if request.include_planning || request.include_drift {
            let scope = PlanningScopeQuery {
                context_id: request.context_id.clone(),
                task_id: request.task_id.clone(),
                agent_package: request.agent_package.clone(),
                agent_id: request.agent_id.clone(),
                history_limit: request.planning_history_limit.max(1) as usize,
            };
            let (all_task_ids, tasks) = self.query_planning_batch(&scope).await?;
            Some(LoadedPlanningSlice {
                all_task_ids,
                tasks,
            })
        } else {
            None
        };

        let ops_filters = ProvenanceOpsFilters {
            context_id: Some(request.context_id.clone()),
            task_id: request.task_id.clone(),
            agent_package: request.agent_package.clone(),
            agent_id: request.agent_id.clone(),
            ..Default::default()
        };

        let llm_filters = ops_filters.clone();
        let tool_filters = ops_filters;

        let llm_future = async {
            if !request.include_llm_ops {
                return Ok(None);
            }
            self.query_live_ops_bundle(
                ProvenanceOpsResource::LlmCalls,
                llm_filters,
                request.ops_page_size,
            )
            .await
            .map(Some)
        };

        let tool_future = async {
            if !request.include_tool_ops {
                return Ok(None);
            }
            self.query_live_ops_bundle(
                ProvenanceOpsResource::ToolCalls,
                tool_filters,
                request.ops_page_size,
            )
            .await
            .map(Some)
        };

        let (llm_ops, tool_ops) = tokio::join!(llm_future, tool_future);
        let llm_ops = llm_ops?;
        let tool_ops = tool_ops?;

        let version = observation_version_from_bundle(&planning, &llm_ops, &tool_ops);
        Ok(LoadedObservationBundle {
            context_id: request.context_id,
            version,
            planning,
            llm_ops,
            tool_ops,
        })
    }

    async fn query_live_ops_bundle(
        &self,
        resource: ProvenanceOpsResource,
        filters: ProvenanceOpsFilters,
        page_size: u32,
    ) -> Result<ProvenanceOpsQueryResponse> {
        let group_by = match resource {
            ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates => {
                vec![
                    "agent_id".to_string(),
                    "agent_package".to_string(),
                    "agent_version".to_string(),
                    "model".to_string(),
                ]
            }
            ProvenanceOpsResource::ToolCalls => vec![
                "agent_id".to_string(),
                "agent_package".to_string(),
                "agent_version".to_string(),
                "tool_name".to_string(),
            ],
            ProvenanceOpsResource::Messages | ProvenanceOpsResource::LifecycleEvents => vec![],
        };
        let request = ProvenanceOpsQueryRequest {
            resource,
            filters: filters.clone(),
            group_by,
            sort_by: Some("timestamp_ms".to_string()),
            sort_dir: Some("desc".to_string()),
            page_size: Some(page_size),
            outcome: Some(ProvenanceOutcomeSegment::Both),
            response_profile: Some(ProvenanceResponseProfile::ToolCompact),
            budget_mode: true,
            paginate_rows_in_sql: true,
            ..Default::default()
        };
        let mode = match observation_scope_from_ops_filters(&filters) {
            Some(scope) => OpsQueryMode::ContextScoped { scope, request },
            None => OpsQueryMode::Global(request),
        };
        ObservationLoader::query_ops(self, mode).await
    }
}
