//! Provenance operations query service contract for UI and system tooling.

use std::{error::Error, fmt};

use baml_rt_provenance::{ProvenanceOpsQueryRequest, ProvenanceOpsQueryResponse};

#[derive(Debug)]
pub enum ProvenanceOpsError {
    NotFound,
    Unavailable,
    Other(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for ProvenanceOpsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "no provenance rows found for the query"),
            Self::Unavailable => write!(f, "provenance ops service unavailable"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl Error for ProvenanceOpsError {}

#[async_trait::async_trait]
pub trait ProvenanceOpsService: Send + Sync {
    async fn query(
        &self,
        request: ProvenanceOpsQueryRequest,
    ) -> Result<ProvenanceOpsQueryResponse, ProvenanceOpsError>;
}
