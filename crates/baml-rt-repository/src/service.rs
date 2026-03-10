//! Repository service — orchestrates storage, hashing, and lineage.
//!
//! This is the primary API surface for the repository. It owns the stores and
//! coordinates multi-step operations (publish, fork, search) that span blob
//! storage, metadata, and lineage.

use std::sync::Arc;

use baml_rt_hash::{CanonicalHasher, HashInput, HashInputFile};

use crate::commands::{ForkCommand, PublishCommand, PublishOrigin, PublishResult};
use crate::entry::{
    RepositoryEntry, RepositoryEntryHeader, SourceBundle, Tag, Timestamp,
};
use crate::error::{RepositoryError, Result};
use crate::ids::{AgentName, ContentHash, Generation, LineageEdgeId, Version, VersionRef};
use crate::lineage::{
    EdgeDescription, LineageEdge, LineageKind, LineageSubgraph,
    Parentage,
};
use crate::search::SearchQuery;
use crate::storage::{BlobStore, LineageStore, MetadataStore, SearchStore};

/// The main repository service.
///
/// Coordinates blob storage, metadata, lineage, and search stores to
/// implement publish, fork, search, and retrieval operations.
pub struct RepositoryService {
    blobs: Arc<dyn BlobStore>,
    metadata: Arc<dyn MetadataStore>,
    lineage: Arc<dyn LineageStore>,
    search: Arc<dyn SearchStore>,
}

impl RepositoryService {
    /// Create a new service with the given store implementations.
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        metadata: Arc<dyn MetadataStore>,
        lineage: Arc<dyn LineageStore>,
        search: Arc<dyn SearchStore>,
    ) -> Self {
        Self {
            blobs,
            metadata,
            lineage,
            search,
        }
    }

    // -----------------------------------------------------------------------
    // Publish
    // -----------------------------------------------------------------------

    /// Publish a new agent version.
    ///
    /// 1. Compute canonical hash from source bundle.
    /// 2. Assign next version number.
    /// 3. Determine parentage and generation from origin.
    /// 4. Insert metadata and lineage edges.
    ///
    /// Does **not** write a blob (the caller packages the tar.gz separately
    /// via `put_blob`).
    pub async fn publish(&self, cmd: PublishCommand) -> Result<PublishResult> {
        let _span = crate::spans::publish(cmd.name.as_str());

        let hash = compute_hash(&cmd.source);
        let version = self.metadata.next_version(&cmd.name).await?;

        let (parentage, generation, edges) = match cmd.origin {
            PublishOrigin::Original => (Parentage::Original, Generation::ROOT, vec![]),

            PublishOrigin::Iteration => {
                // Find previous version to create implicit fork edge
                let prev_version_num = version.as_u32().checked_sub(1);
                match prev_version_num {
                    Some(0) | None => {
                        // First version — treat as original
                        (Parentage::Original, Generation::ROOT, vec![])
                    }
                    Some(prev_v) => {
                        let prev_ver = Version::new(prev_v).expect("checked > 0");
                        let prev_ref = VersionRef {
                            name: cmd.name.clone(),
                            version: prev_ver,
                        };
                        match self.metadata.resolve_hash(&prev_ref).await? {
                            Some(prev_hash) => {
                                let prev_entry =
                                    self.metadata.get_by_hash(&prev_hash).await?;
                                let prev_gen = prev_entry
                                    .map(|e| e.generation)
                                    .unwrap_or(Generation::ROOT);
                                let edge = LineageEdge {
                                    id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                                    source: prev_hash.clone(),
                                    target: hash.clone(),
                                    kind: LineageKind::Fork,
                                    description: EdgeDescription::new(format!(
                                        "Iteration from {name}@{prev_ver}",
                                        name = cmd.name
                                    ))
                                    .expect("non-empty"),
                                };
                                (
                                    Parentage::Forked {
                                        parent: prev_hash,
                                        description: edge.description.clone(),
                                    },
                                    prev_gen,
                                    vec![edge],
                                )
                            }
                            None => (Parentage::Original, Generation::ROOT, vec![]),
                        }
                    }
                }
            }

            PublishOrigin::Influenced { influences } => {
                let mut edges = Vec::new();
                let mut max_gen = Generation::ROOT;

                for inf in &influences {
                    // Verify source exists
                    let source_entry = self.metadata.get_by_hash(&inf.source).await?;
                    match source_entry {
                        Some(e) => {
                            if e.generation > max_gen {
                                max_gen = e.generation;
                            }
                        }
                        None => {
                            return Err(RepositoryError::InfluenceSourceNotFound {
                                source_hash: inf.source.clone(),
                            });
                        }
                    }
                    edges.push(LineageEdge {
                        id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                        source: inf.source.clone(),
                        target: hash.clone(),
                        kind: LineageKind::Influence,
                        description: inf.description.clone(),
                    });
                }

                (
                    Parentage::Synthesized {
                        influences: influences.clone(),
                    },
                    max_gen.increment(),
                    edges,
                )
            }
        };

        let now = chrono_now();
        let version_ref = VersionRef {
            name: cmd.name.clone(),
            version,
        };

        let entry = RepositoryEntry {
            hash: hash.clone(),
            version_ref: version_ref.clone(),
            source: cmd.source,
            parentage: parentage.clone(),
            generation,
            change_rationale: cmd.rationale,
            created_at: now,
            fitness_scores: vec![],
            tags: cmd.tags,
        };

        self.metadata.insert_entry(&entry).await?;

        if !edges.is_empty() {
            self.lineage.record_edges(&edges).await?;
        }

        tracing::info!(
            hash = %hash,
            version = %version_ref,
            generation = generation.as_u32(),
            event = "published"
        );

        Ok(PublishResult {
            hash,
            version_ref,
            generation,
        })
    }

    // -----------------------------------------------------------------------
    // Fork
    // -----------------------------------------------------------------------

    /// Fork an existing entry into a new lineage.
    pub async fn fork(&self, cmd: ForkCommand) -> Result<PublishResult> {
        let _span = crate::spans::fork(cmd.source_hash.as_str(), cmd.new_name.as_str());

        // Verify source exists
        let source_entry = self
            .metadata
            .get_by_hash(&cmd.source_hash)
            .await?
            .ok_or_else(|| RepositoryError::ForkParentNotFound {
                parent_hash: cmd.source_hash.clone(),
            })?;

        let hash = compute_hash(&cmd.source);
        let version = Version::FIRST;
        let generation = source_entry.generation.increment();

        let version_ref = VersionRef {
            name: cmd.new_name.clone(),
            version,
        };

        let parentage = Parentage::Forked {
            parent: cmd.source_hash.clone(),
            description: cmd.fork_description.clone(),
        };

        let edge = LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: cmd.source_hash.clone(),
            target: hash.clone(),
            kind: LineageKind::Fork,
            description: cmd.fork_description,
        };

        let now = chrono_now();
        let entry = RepositoryEntry {
            hash: hash.clone(),
            version_ref: version_ref.clone(),
            source: cmd.source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            created_at: now,
            fitness_scores: vec![],
            tags: cmd.tags,
        };

        self.metadata.insert_entry(&entry).await?;
        self.lineage.record_edges(&[edge]).await?;

        tracing::info!(
            hash = %hash,
            version = %version_ref,
            forked_from = %cmd.source_hash,
            event = "forked"
        );

        Ok(PublishResult {
            hash,
            version_ref,
            generation,
        })
    }

    // -----------------------------------------------------------------------
    // Retrieval
    // -----------------------------------------------------------------------

    /// Get a full entry by content hash.
    pub async fn get_by_hash(&self, hash: &ContentHash) -> Result<Option<RepositoryEntry>> {
        self.metadata.get_by_hash(hash).await
    }

    /// Get a full entry by name + version.
    pub async fn get_by_version(
        &self,
        name: &AgentName,
        version: Version,
    ) -> Result<Option<RepositoryEntry>> {
        self.metadata.get_by_version(name, version).await
    }

    /// Get the latest version of an agent.
    pub async fn get_latest(&self, name: &AgentName) -> Result<Option<RepositoryEntry>> {
        self.metadata.get_latest(name).await
    }

    /// List all versions of an agent.
    pub async fn list_versions(&self, name: &AgentName) -> Result<Vec<RepositoryEntryHeader>> {
        self.metadata.list_versions(name).await
    }

    /// List all known agent names.
    pub async fn list_agents(&self) -> Result<Vec<AgentName>> {
        self.metadata.list_agents().await
    }

    // -----------------------------------------------------------------------
    // Blob operations
    // -----------------------------------------------------------------------

    /// Store a tar.gz blob.
    pub async fn put_blob(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        self.blobs.put(hash, data).await
    }

    /// Retrieve a tar.gz blob.
    pub async fn get_blob(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        self.blobs.get(hash).await
    }

    // -----------------------------------------------------------------------
    // Lineage
    // -----------------------------------------------------------------------

    /// Get the lineage subgraph centered on an entry.
    pub async fn lineage(
        &self,
        hash: &ContentHash,
        ancestor_depth: u32,
    ) -> Result<LineageSubgraph> {
        self.lineage.subgraph(hash, ancestor_depth).await
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Execute a structured search query.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<RepositoryEntryHeader>> {
        self.search.search(query).await
    }

    /// Get top entries by fitness score.
    pub async fn top_by_fitness(
        &self,
        domain: &crate::entry::FitnessDomain,
        limit: usize,
    ) -> Result<Vec<RepositoryEntryHeader>> {
        self.search.top_by_fitness(domain, limit).await
    }

    // -----------------------------------------------------------------------
    // Metadata mutation
    // -----------------------------------------------------------------------

    /// Record a fitness score for an entry.
    pub async fn record_fitness(
        &self,
        hash: &ContentHash,
        domain: crate::entry::FitnessDomain,
        score: f64,
    ) -> Result<()> {
        let now = chrono_now();
        self.metadata
            .record_fitness(hash, domain, score, now)
            .await
    }

    /// Add a tag to an entry.
    pub async fn add_tag(&self, hash: &ContentHash, tag: Tag) -> Result<()> {
        self.metadata.add_tag(hash, tag).await
    }

    /// Remove a tag from an entry.
    pub async fn remove_tag(&self, hash: &ContentHash, tag: &Tag) -> Result<()> {
        self.metadata.remove_tag(hash, tag).await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the canonical content hash from a source bundle.
fn compute_hash(source: &SourceBundle) -> ContentHash {
    let manifest_value = source.manifest.as_value();
    let ts_files: Vec<HashInputFile<'_>> = source
        .ts_sources
        .iter()
        .map(|f| HashInputFile {
            path: f.path.as_str(),
            content: f.content.as_str(),
        })
        .collect();
    let baml_files: Vec<HashInputFile<'_>> = source
        .baml_sources
        .iter()
        .map(|f| HashInputFile {
            path: f.path.as_str(),
            content: f.content.as_str(),
        })
        .collect();

    let input = HashInput {
        manifest: manifest_value,
        ts_files,
        baml_files,
    };
    CanonicalHasher::hash(&input)
}

/// UTC timestamp in RFC 3339 format.
fn chrono_now() -> Timestamp {
    // Use system time formatted as RFC 3339.
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Simple RFC 3339 formatting without chrono dependency
    Timestamp::new(format!("{secs}"))
}
