//! Storage trait boundaries: hybrid FS + SQLite architecture.
//!
//! The repository uses a split storage model:
//!
//! - **BlobStore** (filesystem): stores distributable tar.gz packages keyed by
//!   `ContentHash`. The filesystem is the source of truth for binary content.
//!
//! - **MetadataStore** (SQLite): stores structured metadata, version mappings,
//!   lineage edges, fitness scores, tags, and full-text search indices. SQLite
//!   is the source of truth for all queryable state.
//!
//! This separation is deliberate:
//! - Tar.gz blobs are opaque and large; SQLite BLOB columns would bloat the
//!   database and degrade query performance.
//! - Metadata is small, structured, and needs indexed search; SQLite excels.
//! - The `ContentHash` is the join key: both stores agree on the hash as the
//!   canonical identity.

use async_trait::async_trait;

use crate::{
    entry::{FitnessDomain, NewEntry, RepositoryEntry, RepositoryEntryHeader, Tag, Timestamp},
    error::Result,
    ids::{AgentName, ContentHash, Version, VersionRef},
    lineage::{AncestryNode, LineageEdge, LineageSubgraph},
    search::SearchQuery,
};

// ---------------------------------------------------------------------------
// BlobStore — filesystem-backed tar.gz storage
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
// MetadataStore — SQLite-backed structured storage
// ---------------------------------------------------------------------------

/// Stores and queries agent metadata, version mappings, and lineage.
///
/// This is the primary interface for all structured data operations. The
/// implementation is expected to be backed by SQLite with appropriate indices
/// for search and lineage traversal.
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

    /// Append a fitness score to an entry. The entry must already exist.
    async fn record_fitness(
        &self,
        hash: &ContentHash,
        domain: FitnessDomain,
        score: f64,
        recorded_at: Timestamp,
    ) -> Result<()>;

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
/// Separated because search indexing may use SQLite FTS5 or an external engine,
/// and has different consistency requirements from core CRUD.
#[async_trait]
pub trait SearchStore: Send + Sync {
    /// Execute a structured search query, returning matching entry headers
    /// ordered by relevance.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<RepositoryEntryHeader>>;

    /// Retrieve the top-k entries by fitness score in a given domain.
    /// This is the ADAS archive hot path.
    async fn top_by_fitness(
        &self,
        domain: &FitnessDomain,
        limit: usize,
    ) -> Result<Vec<RepositoryEntryHeader>>;
}
