//! [`ObservationLoader`] — load once, project operator surfaces.

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;

use super::{
    fingerprint::observation_version_from_loaded,
    ops::project_ops_llm_summary_count,
    types::{LoadedObservation, ObservationScope, ObservationVersion, OpsQueryMode},
};
use crate::{
    error::Result,
    store::{ProvenanceOpsQuery, ProvenanceOpsQueryResponse, ProvenanceOpsResource},
    surreal_store::SurrealProvenanceStore,
};

#[async_trait]
pub trait ObservationLoader: Send + Sync {
    async fn load(&self, scope: ObservationScope) -> Result<LoadedObservation>;

    async fn load_delta(
        &self,
        scope: ObservationScope,
        after: super::types::EventOrder,
        limit: usize,
    ) -> Result<(LoadedObservation, Vec<ProvenanceConversationContextItem>)>;

    async fn query_ops(&self, mode: OpsQueryMode) -> Result<ProvenanceOpsQueryResponse>;

    fn version(&self, obs: &LoadedObservation) -> ObservationVersion {
        observation_version_from_loaded(obs)
    }
}

#[async_trait]
impl ObservationLoader for SurrealProvenanceStore {
    async fn load(&self, scope: ObservationScope) -> Result<LoadedObservation> {
        self.load_observation(scope).await
    }

    async fn load_delta(
        &self,
        scope: ObservationScope,
        after: super::types::EventOrder,
        limit: usize,
    ) -> Result<(LoadedObservation, Vec<ProvenanceConversationContextItem>)> {
        self.load_observation_delta(scope, after, limit).await
    }

    async fn query_ops(&self, mode: OpsQueryMode) -> Result<ProvenanceOpsQueryResponse> {
        match mode {
            OpsQueryMode::Global(request) => ProvenanceOpsQuery::query_ops(self, request).await,
            OpsQueryMode::ContextScoped { scope, request } => {
                let metrics = self.load_task_metrics(&scope).await?;
                let patch_count = matches!(
                    request.resource,
                    ProvenanceOpsResource::LlmCalls | ProvenanceOpsResource::Aggregates
                );
                let mut response = ProvenanceOpsQuery::query_ops(self, request).await?;
                // Task-scoped episode counts must not override filtered row summaries.
                if patch_count
                    && scope.agent_package.is_none()
                    && let Some(metrics) = metrics
                {
                    project_ops_llm_summary_count(&mut response, metrics.llm_call_count);
                }
                Ok(response)
            }
        }
    }
}
