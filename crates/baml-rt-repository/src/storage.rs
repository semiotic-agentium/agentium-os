//! Storage trait boundaries for repository persistence.
//!
//! The repository keeps these concerns separated at the trait level:
//!
//! - `BlobStore` for packaged tar.gz bytes.
//! - `MetadataStore` for entry/version metadata.
//! - `LineageStore` for graph traversal.
//! - `SearchStore` for query/filter/ranking.
//!
//! A concrete backend may implement all traits in one store (as the current
//! SurrealDB backend does), while preserving these boundaries for testability
//! and future evolution.

use async_trait::async_trait;
use baml_rt_tools::mcp_snapshot::McpServerSnapshot;

use crate::{
    entry::{NewEntry, RepositoryEntry, RepositoryEntryHeader, Tag},
    error::Result,
    ids::{AgentName, ContentHash, Version, VersionRef},
    lineage::{AncestryNode, LineageEdge, LineageSubgraph},
    mcp::{McpRegistryServer, McpRegistryServerVersion, McpRegistryToolVersion},
    search::SearchQuery,
};

// ---------------------------------------------------------------------------
// BlobStore — tar.gz storage
// ---------------------------------------------------------------------------

/// Stores and retrieves distributable tar.gz packages by content hash.
///
/// The blob store is content-addressable: the `ContentHash` determines the
/// storage path. The store does not interpret the blob contents.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Write a tar.gz blob for the given hash. Overwrites if already present
    /// (idempotent for identical content).
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> Result<()>;

    /// Retrieve a tar.gz blob by hash. Returns `None` if no blob exists.
    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>>;

    /// Check whether a blob exists without reading it.
    async fn exists(&self, hash: &ContentHash) -> Result<bool>;

    /// Delete a blob. No-op if the blob does not exist.
    async fn delete(&self, hash: &ContentHash) -> Result<()>;
}

// ---------------------------------------------------------------------------
// MetadataStore — structured storage
// ---------------------------------------------------------------------------

/// Stores and queries agent metadata, version mappings, and lineage.
///
/// This is the primary interface for all structured data operations. The
/// implementation is expected to provide appropriate indices for search and
/// lineage traversal.
#[async_trait]
pub trait MetadataStore: Send + Sync {
    // --- Entry lifecycle ---

    /// Insert a new entry, atomically assigning the next version number and
    /// computing the content hash.
    ///
    /// The store:
    /// 1. Assigns the next version for the agent lineage.
    /// 2. Writes the version into the manifest.
    /// 3. Computes the canonical content hash from the versioned source.
    /// 4. Persists the entry.
    ///
    /// Returns the complete `RepositoryEntry` with the assigned version, hash,
    /// and timestamp. Fails if the content hash already exists.
    async fn insert_entry(&self, entry: &NewEntry) -> Result<RepositoryEntry>;

    /// Retrieve a full entry by content hash.
    async fn get_by_hash(&self, hash: &ContentHash) -> Result<Option<RepositoryEntry>>;

    /// Retrieve a full entry by name + version.
    async fn get_by_version(
        &self,
        name: &AgentName,
        version: Version,
    ) -> Result<Option<RepositoryEntry>>;

    /// Retrieve the latest version of an agent by name.
    async fn get_latest(&self, name: &AgentName) -> Result<Option<RepositoryEntry>>;

    /// Resolve a content hash from a version reference.
    async fn resolve_hash(&self, version_ref: &VersionRef) -> Result<Option<ContentHash>>;

    // --- Listings ---

    /// List all entries for a given agent name, ordered by version descending.
    async fn list_versions(&self, name: &AgentName) -> Result<Vec<RepositoryEntryHeader>>;

    /// List all known agent names in the repository.
    async fn list_agents(&self) -> Result<Vec<AgentName>>;

    // --- Mutable metadata ---

    /// Append a tag to an entry. Idempotent (duplicate tags are ignored).
    async fn add_tag(&self, hash: &ContentHash, tag: Tag) -> Result<()>;

    /// Remove a tag from an entry. No-op if the tag does not exist.
    async fn remove_tag(&self, hash: &ContentHash, tag: &Tag) -> Result<()>;
}

// ---------------------------------------------------------------------------
// LineageStore — DAG traversal and edge management
// ---------------------------------------------------------------------------

/// Lineage-specific storage operations.
///
/// Separated from `MetadataStore` because lineage queries have distinct access
/// patterns (graph traversal vs flat lookup) and may warrant separate indices.
#[async_trait]
pub trait LineageStore: Send + Sync {
    /// Record lineage edges for a newly published entry.
    ///
    /// Called during publish/fork. All referenced source hashes must already
    /// exist in the metadata store.
    async fn record_edges(&self, edges: &[LineageEdge]) -> Result<()>;

    /// Retrieve all direct parents of an entry (both fork and influence).
    async fn parents(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>>;

    /// Retrieve all direct children (entries that cite this hash as parent/influence).
    async fn children(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>>;

    /// Walk ancestors up to `max_depth` levels. Returns nodes ordered
    /// root-most first.
    async fn ancestors(&self, hash: &ContentHash, max_depth: u32) -> Result<Vec<AncestryNode>>;

    /// Retrieve all entries that list this hash as an influence.
    async fn influenced_by(&self, hash: &ContentHash) -> Result<Vec<AncestryNode>>;

    /// Build a local subgraph centered on an entry, including ancestors up to
    /// `ancestor_depth` and direct descendants.
    async fn subgraph(&self, hash: &ContentHash, ancestor_depth: u32) -> Result<LineageSubgraph>;
}

// ---------------------------------------------------------------------------
// SearchStore — full-text and metadata search
// ---------------------------------------------------------------------------

/// Search operations over repository metadata and source content.
///
/// Separated because search indexing may use backend-specific query/index
/// capabilities and has different consistency requirements from core CRUD.
#[async_trait]
pub trait SearchStore: Send + Sync {
    /// Execute a structured search query, returning matching entry headers
    /// ordered by relevance.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<RepositoryEntryHeader>>;
}

// ---------------------------------------------------------------------------
// McpRegistryStore — MCP server snapshot catalog
// ---------------------------------------------------------------------------

/// Stores immutable MCP server snapshot versions and their tool projections.
#[async_trait]
pub trait McpRegistryStore: Send + Sync {
    /// List known MCP servers ordered by id.
    async fn list_mcp_servers(&self) -> Result<Vec<McpRegistryServer>>;

    /// Insert a full server snapshot as a new immutable version.
    async fn put_mcp_snapshot(
        &self,
        snapshot: &McpServerSnapshot,
    ) -> Result<McpRegistryServerVersion>;

    /// Retrieve a full server snapshot by server id and registry version.
    async fn get_mcp_snapshot(
        &self,
        server_id: &str,
        version: u32,
    ) -> Result<Option<McpServerSnapshot>>;

    /// Retrieve the latest server snapshot for a server id.
    async fn get_latest_mcp_snapshot(&self, server_id: &str) -> Result<Option<McpServerSnapshot>>;

    /// List registry versions for one server id, newest first.
    async fn list_mcp_server_versions(
        &self,
        server_id: &str,
    ) -> Result<Vec<McpRegistryServerVersion>>;

    /// Find all tool-version rows for a platform tool name.
    async fn find_mcp_tool(&self, platform_tool_name: &str) -> Result<Vec<McpRegistryToolVersion>>;

    /// Mark a server version stale.
    async fn mark_mcp_version_stale(&self, server_id: &str, version: u32) -> Result<()>;
}
