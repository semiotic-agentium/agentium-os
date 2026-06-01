// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Ops page reads (delegates to [`crate::store::ProvenanceOpsQuery`] with budget semantics).

use async_trait::async_trait;

use crate::{
    error::Result,
    store::{ProvenanceOpsQueryRequest, ProvenanceOpsQueryResponse},
};

#[derive(Debug, Clone)]
pub struct OpsPageSpec {
    pub request: ProvenanceOpsQueryRequest,
}

#[async_trait]
pub trait OpsReader: Send + Sync {
    async fn page(&self, spec: OpsPageSpec) -> Result<ProvenanceOpsQueryResponse>;
}
