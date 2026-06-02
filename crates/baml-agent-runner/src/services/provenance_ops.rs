// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Provenance ops query service with explicit global vs context-scoped modes.

use std::sync::Arc;

use baml_rt_provenance::{
    ObservationLoader as _, OpsQueryMode, observation_scope_from_ops_filters,
};

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
        let mode = match observation_scope_from_ops_filters(&request.filters) {
            Some(scope) => OpsQueryMode::ContextScoped { scope, request },
            None => OpsQueryMode::Global(request),
        };
        self.store
            .query_ops(mode)
            .await
            .map_err(|e| baml_rt_api::ProvenanceOpsError::Other(Box::new(std::io::Error::other(e))))
    }
}
