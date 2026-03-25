//! HTTP API types for the repository service.
//!
//! Request and response types for the REST API surface, plus RFC 7807
//! error mapping from `RepositoryError`.

use http_api_problem::{HttpApiProblem, StatusCode as ProblemStatusCode};
use serde::{Deserialize, Serialize};

use crate::{
    entry::RepositoryEntryHeader,
    error::RepositoryError,
    ids::{AgentName, ContentHash, Version},
    lineage::LineageSubgraph,
};

// ---------------------------------------------------------------------------
// Problem type constants (RFC 7807)
// ---------------------------------------------------------------------------

/// Relative URIs for repository-specific error types.
pub mod problem_types {
    pub const ENTRY_NOT_FOUND: &str = "/problems/entry-not-found";
    pub const LINEAGE_NOT_FOUND: &str = "/problems/lineage-not-found";
    pub const DUPLICATE_HASH: &str = "/problems/duplicate-hash";
    pub const VERSION_CONFLICT: &str = "/problems/version-conflict";
    pub const INVALID_SOURCE: &str = "/problems/invalid-source-bundle";
    pub const HASH_MISMATCH: &str = "/problems/hash-mismatch";
    pub const LINEAGE_VIOLATION: &str = "/problems/lineage-violation";
    pub const STORAGE_ERROR: &str = "/problems/storage-error";
    pub const SEARCH_ERROR: &str = "/problems/search-error";
}

// ---------------------------------------------------------------------------
// RepositoryError → HttpApiProblem
// ---------------------------------------------------------------------------

impl From<RepositoryError> for HttpApiProblem {
    fn from(e: RepositoryError) -> Self {
        use problem_types::*;

        match &e {
            RepositoryError::EntryNotFoundByHash { .. }
            | RepositoryError::EntryNotFoundByVersion { .. } => {
                HttpApiProblem::new(ProblemStatusCode::NOT_FOUND)
                    .title("Entry not found")
                    .type_url(ENTRY_NOT_FOUND)
                    .detail(e.to_string())
            }

            RepositoryError::LineageNotFound { .. } => {
                HttpApiProblem::new(ProblemStatusCode::NOT_FOUND)
                    .title("Lineage not found")
                    .type_url(LINEAGE_NOT_FOUND)
                    .detail(e.to_string())
            }

            RepositoryError::DuplicateHash { .. } => {
                HttpApiProblem::new(ProblemStatusCode::CONFLICT)
                    .title("Duplicate content hash")
                    .type_url(DUPLICATE_HASH)
                    .detail(e.to_string())
            }

            RepositoryError::VersionConflict { .. } => {
                HttpApiProblem::new(ProblemStatusCode::CONFLICT)
                    .title("Version conflict")
                    .type_url(VERSION_CONFLICT)
                    .detail(e.to_string())
            }

            RepositoryError::InvalidSourceBundle { .. } => {
                HttpApiProblem::new(ProblemStatusCode::BAD_REQUEST)
                    .title("Invalid source bundle")
                    .type_url(INVALID_SOURCE)
                    .detail(e.to_string())
            }

            RepositoryError::BlobTooLarge { .. } => {
                HttpApiProblem::new(ProblemStatusCode::BAD_REQUEST)
                    .title("Blob too large")
                    .type_url(INVALID_SOURCE)
                    .detail(e.to_string())
            }

            RepositoryError::HashMismatch { .. } => {
                HttpApiProblem::new(ProblemStatusCode::BAD_REQUEST)
                    .title("Hash mismatch")
                    .type_url(HASH_MISMATCH)
                    .detail(e.to_string())
            }

            RepositoryError::ForkParentNotFound { .. }
            | RepositoryError::InfluenceSourceNotFound { .. }
            | RepositoryError::LineageCycle { .. } => {
                HttpApiProblem::new(ProblemStatusCode::UNPROCESSABLE_ENTITY)
                    .title("Lineage violation")
                    .type_url(LINEAGE_VIOLATION)
                    .detail(e.to_string())
            }

            RepositoryError::StorageWrite { .. } | RepositoryError::StorageRead { .. } => {
                HttpApiProblem::new(ProblemStatusCode::INTERNAL_SERVER_ERROR)
                    .title("Storage error")
                    .type_url(STORAGE_ERROR)
                    .detail("Internal storage operation failed")
            }

            RepositoryError::SearchExecution { .. } => {
                HttpApiProblem::new(ProblemStatusCode::INTERNAL_SERVER_ERROR)
                    .title("Search error")
                    .type_url(SEARCH_ERROR)
                    .detail("Search operation failed")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP result alias
// ---------------------------------------------------------------------------

/// Standard HTTP result type for repository handlers.
pub type HttpResult<T> = std::result::Result<axum::Json<T>, HttpApiProblem>;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// GET /entries/:hash
#[derive(Debug, Deserialize)]
pub struct GetByHashPath {
    pub hash: String,
}

/// GET /entries?name=<name>&version=<version>
#[derive(Debug, Deserialize)]
pub struct EntriesQuery {
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug)]
pub enum EntriesQueryMode {
    All,
    ByName(AgentName),
    ByNameVersion { name: AgentName, version: Version },
}

impl TryFrom<EntriesQuery> for EntriesQueryMode {
    type Error = String;

    fn try_from(value: EntriesQuery) -> std::result::Result<Self, Self::Error> {
        let Some(name_raw) = value.name else {
            if value.version.is_some() {
                return Err("version query requires name".to_string());
            }
            return Ok(Self::All);
        };
        let name = name_raw.parse::<AgentName>().map_err(|e| e.to_string())?;
        match value.version {
            Some(version_raw) => {
                let version = version_raw.parse::<Version>().map_err(|e| e.to_string())?;
                Ok(Self::ByNameVersion { name, version })
            }
            None => Ok(Self::ByName(name)),
        }
    }
}

/// PUT/GET /blobs/:hash
#[derive(Debug, Deserialize)]
pub struct BlobPath {
    pub hash: String,
}

/// GET /entries/:name/:version
#[derive(Debug, Deserialize)]
pub struct GetByVersionPath {
    pub name: String,
    pub version: String,
}

/// GET /agents/:name/versions
#[derive(Debug, Deserialize)]
pub struct ListVersionsPath {
    pub name: String,
}

/// GET /lineage/:hash
#[derive(Debug, Deserialize)]
pub struct LineagePath {
    pub hash: String,
}

/// GET /lineage/:hash?depth=N
#[derive(Debug, Deserialize)]
pub struct LineageQuery {
    #[serde(default = "default_lineage_depth")]
    pub depth: u32,
}

fn default_lineage_depth() -> u32 {
    5
}

/// POST /entries/:hash/tags
#[derive(Debug, Deserialize)]
pub struct TagPath {
    pub hash: String,
}

/// Request body for adding a tag.
#[derive(Debug, Deserialize)]
pub struct AddTagRequest {
    pub tag: String,
}

/// Request body for removing a tag.
#[derive(Debug, Deserialize)]
pub struct RemoveTagRequest {
    pub tag: String,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Response for search results.
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<RepositoryEntryHeader>,
    pub total: usize,
}

/// Response for list entries endpoint.
#[derive(Debug, Serialize)]
pub struct EntriesResponse {
    pub entries: Vec<RepositoryEntryHeader>,
    pub total: usize,
}

/// Response for agent name listing.
#[derive(Debug, Serialize)]
pub struct ListAgentsResponse {
    pub agents: Vec<AgentName>,
}

/// Response for version listing.
#[derive(Debug, Serialize)]
pub struct ListVersionsResponse {
    pub name: AgentName,
    pub versions: Vec<RepositoryEntryHeader>,
}

/// Response for lineage queries.
#[derive(Debug, Serialize)]
pub struct LineageResponse {
    pub subgraph: LineageSubgraph,
}

/// Response metadata for blob download.
#[derive(Debug, Serialize)]
pub struct BlobDownloadMeta {
    pub hash: ContentHash,
    pub size_bytes: usize,
}
