//! [`OpsReader`] — budget-aware ops paging.

use async_trait::async_trait;

use super::SurrealProvenanceStore;
use crate::{
    error::Result,
    read::{OpsPageSpec, OpsReader},
    store::{ProvenanceOpsQuery, ProvenanceResponseProfile},
};

#[async_trait]
impl OpsReader for SurrealProvenanceStore {
    async fn page(&self, spec: OpsPageSpec) -> Result<crate::store::ProvenanceOpsQueryResponse> {
        let mut request = spec.request;
        if request.budget_mode {
            request.response_profile = Some(ProvenanceResponseProfile::ToolCompact);
        }
        ProvenanceOpsQuery::query_ops(self, request).await
    }
}
