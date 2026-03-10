//! Repository service — orchestrates storage, hashing, and lineage.
//!
//! This is the primary API surface for the repository. It owns the stores and
//! coordinates multi-step operations (publish, fork, search) that span blob
//! storage, metadata, and lineage.

use std::sync::Arc;

use baml_rt_hash::{CanonicalHasher, HashInput, HashInputFile};

use crate::{
    commands::{ForkCommand, PublishCommand, PublishOrigin, PublishResult},
    entry::{NewEntry, RepositoryEntry, RepositoryEntryHeader, SourceBundle, Tag, Timestamp},
    error::{RepositoryError, Result},
    ids::{AgentName, ContentHash, Generation, LineageEdgeId, Version},
    lineage::{EdgeDescription, LineageEdge, LineageKind, LineageSubgraph, Parentage},
    search::SearchQuery,
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
};

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
    /// 2. Determine parentage and generation from origin.
    /// 3. Insert metadata (store atomically assigns version).
    /// 4. Record lineage edges.
    ///
    /// Does **not** write a blob (the caller packages the tar.gz separately
    /// via `put_blob`).
    pub async fn publish(&self, cmd: PublishCommand) -> Result<PublishResult> {
        let _span = crate::spans::publish(cmd.name.as_str());

        let hash = compute_hash(&cmd.source);

        let (parentage, generation, deferred_edges) = match cmd.origin {
            PublishOrigin::Original => (Parentage::Original, Generation::ROOT, vec![]),

            PublishOrigin::Iteration => {
                // Look up the latest existing version to create an implicit fork edge.
                match self.metadata.get_latest(&cmd.name).await? {
                    Some(prev) => {
                        let edge = LineageEdge {
                            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                            source: prev.hash.clone(),
                            target: hash.clone(),
                            kind: LineageKind::Fork,
                            description: EdgeDescription::new(format!(
                                "Iteration from {ver}",
                                ver = prev.version_ref
                            ))
                            .expect("non-empty"),
                        };
                        (
                            Parentage::Forked {
                                parent: prev.hash,
                                description: edge.description.clone(),
                            },
                            prev.generation,
                            vec![edge],
                        )
                    }
                    None => {
                        // No prior version — treat as original
                        (Parentage::Original, Generation::ROOT, vec![])
                    }
                }
            }

            PublishOrigin::Influenced { influences } => {
                let mut edges = Vec::new();
                let mut max_gen = Generation::ROOT;

                for inf in &influences {
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

        let new_entry = NewEntry {
            hash: hash.clone(),
            name: cmd.name.clone(),
            source: cmd.source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            tags: cmd.tags,
        };

        // Store atomically assigns the version number.
        let stored = self.metadata.insert_entry(&new_entry).await?;

        if !deferred_edges.is_empty() {
            self.lineage.record_edges(&deferred_edges).await?;
        }

        tracing::info!(
            hash = %stored.hash,
            version = %stored.version_ref,
            generation = stored.generation.as_u32(),
            event = "published"
        );

        Ok(PublishResult {
            hash: stored.hash,
            version_ref: stored.version_ref,
            generation: stored.generation,
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
        let generation = source_entry.generation.increment();

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

        let new_entry = NewEntry {
            hash: hash.clone(),
            name: cmd.new_name.clone(),
            source: cmd.source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            tags: cmd.tags,
        };

        // Store atomically assigns version (v1 for a new lineage).
        let stored = self.metadata.insert_entry(&new_entry).await?;
        self.lineage.record_edges(&[edge]).await?;

        tracing::info!(
            hash = %stored.hash,
            version = %stored.version_ref,
            forked_from = %cmd.source_hash,
            event = "forked"
        );

        Ok(PublishResult {
            hash: stored.hash,
            version_ref: stored.version_ref,
            generation: stored.generation,
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
        self.metadata.record_fitness(hash, domain, score, now).await
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
pub(crate) fn chrono_now() -> Timestamp {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    Timestamp::new(format!("{secs}"))
}
