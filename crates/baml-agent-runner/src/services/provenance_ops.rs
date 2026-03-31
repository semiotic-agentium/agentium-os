//! Provenance ops service (delegating query to SurrealDB store).

use std::sync::Arc;

use baml_rt_provenance::ProvenanceOpsQuery as _;

pub(crate) struct ProvenanceOpsServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl ProvenanceOpsServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl baml_rt_api::ProvenanceOpsService for ProvenanceOpsServiceImpl {
    async fn query(
        &self,
        request: baml_rt_provenance::ProvenanceOpsQueryRequest,
    ) -> std::result::Result<
        baml_rt_provenance::ProvenanceOpsQueryResponse,
        baml_rt_api::ProvenanceOpsError,
    > {
        self.store
            .query_ops(request)
            .await
            .map_err(|e| baml_rt_api::ProvenanceOpsError::Other(Box::new(std::io::Error::other(e))))
    }
}
