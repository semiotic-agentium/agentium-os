//! Error types for the agent repository.
//!
//! Error variant names describe the *operation* that failed, not the error
//! source. Structured fields carry diagnostic context without stringifying
//! the original error.

use thiserror::Error;

use crate::ids::{AgentName, ContentHash, Version};

/// Repository operation errors.
#[derive(Error, Debug)]
pub enum RepositoryError {
    // --- Lookup failures ---
    #[error("Agent entry not found by hash: {hash}")]
    EntryNotFoundByHash { hash: ContentHash },

    #[error("Agent entry not found: {name}@{version}")]
    EntryNotFoundByVersion { name: AgentName, version: Version },

    #[error("Agent lineage not found: {name}")]
    LineageNotFound { name: AgentName },

    // --- Conflict / invariant violations ---
    #[error(
        "Duplicate content hash: {hash} (content already exists as {existing_name}@{existing_version})"
    )]
    DuplicateHash {
        hash: ContentHash,
        existing_name: AgentName,
        existing_version: Version,
    },

    #[error("Version conflict: {name}@{version} already exists")]
    VersionConflict { name: AgentName, version: Version },

    // --- Validation failures ---
    #[error("Invalid source bundle: {reason}")]
    InvalidSourceBundle { reason: String },

    #[error("Blob too large: size={size_bytes} bytes exceeds max={max_bytes} bytes")]
    BlobTooLarge { size_bytes: usize, max_bytes: usize },

    #[error("Canonical hash mismatch: expected {expected}, computed {computed}")]
    HashMismatch {
        expected: ContentHash,
        computed: ContentHash,
    },

    // --- Lineage violations ---
    #[error("Fork parent not found: {parent_hash}")]
    ForkParentNotFound { parent_hash: ContentHash },

    #[error("Influence source not found: {source_hash}")]
    InfluenceSourceNotFound { source_hash: ContentHash },

    #[error("Lineage cycle detected: {hash} would create a cycle")]
    LineageCycle { hash: ContentHash },

    // --- Storage ---
    #[error("Storage operation failed")]
    StorageWrite {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Storage read failed")]
    StorageRead {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    // --- Search ---
    #[error("Search query failed")]
    SearchExecution {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Convenience alias for repository operations.
pub type Result<T> = std::result::Result<T, RepositoryError>;
