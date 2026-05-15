//! Repository service — orchestrates storage, hashing, and lineage.
//!
//! This is the primary API surface for the repository. It owns the stores and
//! coordinates multi-step operations (publish, fork, search) that span blob
//! storage, metadata, and lineage.

use std::sync::Arc;

use baml_rt_core::clock_events;
use baml_rt_tools::mcp_snapshot::McpServerSnapshot;

use crate::{
    commands::{ForkCommand, PublishCommand, PublishOrigin, PublishResult},
    entry::{NewEntry, RepositoryEntry, RepositoryEntryHeader, Tag, Timestamp},
    error::{RepositoryError, Result},
    ids::{AgentName, ContentHash, Generation, LineageEdgeId, Version},
    lineage::{EdgeDescription, LineageEdge, LineageKind, LineageSubgraph, Parentage},
    mcp::{McpRegistryServerVersion, McpRegistryToolVersion},
    search::SearchQuery,
    storage::{BlobStore, LineageStore, McpRegistryStore, MetadataStore, SearchStore},
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
    mcp_registry: Arc<dyn McpRegistryStore>,
}

impl RepositoryService {
    /// Create a new service with the given store implementations.
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        metadata: Arc<dyn MetadataStore>,
        lineage: Arc<dyn LineageStore>,
        search: Arc<dyn SearchStore>,
        mcp_registry: Arc<dyn McpRegistryStore>,
    ) -> Self {
        Self {
            blobs,
            metadata,
            lineage,
            search,
            mcp_registry,
        }
    }

    // -----------------------------------------------------------------------
    // MCP registry
    // -----------------------------------------------------------------------

    pub async fn put_mcp_snapshot(
        &self,
        snapshot: &McpServerSnapshot,
    ) -> Result<McpRegistryServerVersion> {
        self.mcp_registry.put_mcp_snapshot(snapshot).await
    }

    pub async fn get_mcp_snapshot(
        &self,
        server_id: &str,
        version: u32,
    ) -> Result<Option<McpServerSnapshot>> {
        self.mcp_registry.get_mcp_snapshot(server_id, version).await
    }

    pub async fn get_latest_mcp_snapshot(
        &self,
        server_id: &str,
    ) -> Result<Option<McpServerSnapshot>> {
        self.mcp_registry.get_latest_mcp_snapshot(server_id).await
    }

    pub async fn list_mcp_server_versions(
        &self,
        server_id: &str,
    ) -> Result<Vec<McpRegistryServerVersion>> {
        self.mcp_registry.list_mcp_server_versions(server_id).await
    }

    pub async fn find_mcp_tool(
        &self,
        platform_tool_name: &str,
    ) -> Result<Vec<McpRegistryToolVersion>> {
        self.mcp_registry.find_mcp_tool(platform_tool_name).await
    }

    pub async fn mark_mcp_version_stale(&self, server_id: &str, version: u32) -> Result<()> {
        self.mcp_registry
            .mark_mcp_version_stale(server_id, version)
            .await
    }

    // -----------------------------------------------------------------------
    // Publish
    // -----------------------------------------------------------------------

    /// Publish a new agent version.
    ///
    /// 1. Determine parentage and generation from origin.
    /// 2. Insert metadata (store atomically assigns version, writes it into
    ///    the manifest, and computes the content hash).
    /// 3. Record lineage edges (using the hash returned by the store).
    pub async fn publish(&self, cmd: PublishCommand) -> Result<PublishResult> {
        let _span = crate::spans::publish(cmd.name.as_str());

        // Determine parentage, generation, and deferred edge descriptors.
        // Edge targets will be filled with stored.hash after insert.
        let (parentage, generation, edge_descriptors) = match cmd.origin {
            PublishOrigin::Original => (Parentage::Original, Generation::ROOT, vec![]),

            PublishOrigin::Iteration => {
                match self.metadata.get_latest(&cmd.name).await? {
                    Some(prev) => {
                        let desc = EdgeDescription::new(format!(
                            "Iteration from {ver}",
                            ver = prev.version_ref
                        ))
                        .expect("non-empty");
                        (
                            Parentage::Forked {
                                parent: prev.hash.clone(),
                                description: desc.clone(),
                            },
                            prev.generation,
                            // (source_hash, kind, description) — target filled later
                            vec![(prev.hash, LineageKind::Fork, desc)],
                        )
                    }
                    None => (Parentage::Original, Generation::ROOT, vec![]),
                }
            }

            PublishOrigin::Influenced { influences } => {
                let mut descs = Vec::new();
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
                    descs.push((
                        inf.source.clone(),
                        LineageKind::Influence,
                        inf.description.clone(),
                    ));
                }

                (
                    Parentage::Synthesized {
                        influences: influences.clone(),
                    },
                    max_gen.increment(),
                    descs,
                )
            }
        };

        let source = cmd.source;
        let tags = manifest_tags(&source);
        let new_entry = NewEntry {
            name: cmd.name.clone(),
            source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            tags,
        };

        // Store atomically assigns version, writes it into manifest, computes hash.
        let stored = self.metadata.insert_entry(&new_entry).await?;

        // Now create lineage edges using the store-assigned hash as target.
        if !edge_descriptors.is_empty() {
            let edges: Vec<LineageEdge> = edge_descriptors
                .into_iter()
                .map(|(source, kind, description)| LineageEdge {
                    id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                    source,
                    target: stored.hash.clone(),
                    kind,
                    description,
                })
                .collect();
            self.lineage.record_edges(&edges).await?;
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

        let generation = source_entry.generation.increment();

        let parentage = Parentage::Forked {
            parent: cmd.source_hash.clone(),
            description: cmd.fork_description.clone(),
        };

        let new_entry = NewEntry {
            name: cmd.new_name.clone(),
            source: cmd.source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            tags: cmd.tags,
        };

        // Store atomically assigns version (v1 for a new lineage), writes it
        // into manifest, computes hash.
        let stored = self.metadata.insert_entry(&new_entry).await?;

        // Record fork edge using the store-assigned hash.
        let edge = LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: cmd.source_hash.clone(),
            target: stored.hash.clone(),
            kind: LineageKind::Fork,
            description: cmd.fork_description,
        };
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

    /// Version number the next [`publish`](Self::publish) for this agent will receive
    /// (matches [`MetadataStore::insert_entry`](crate::storage::MetadataStore::insert_entry) logic).
    pub async fn next_version_for_agent(&self, name: &AgentName) -> Result<u32> {
        Ok(match self.get_latest(name).await? {
            Some(prev) => prev.version_ref.version.as_u32().saturating_add(1),
            None => 1,
        })
    }

    // -----------------------------------------------------------------------
    // Blob operations
    // -----------------------------------------------------------------------

    /// Retrieve a tar.gz blob.
    pub async fn get_blob(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        self.blobs.get(hash).await
    }

    /// Store a repository-built tar.gz blob under content hash.
    ///
    /// This is intended for trusted in-process orchestrators (not public HTTP upload).
    pub async fn put_built_blob(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        self.blobs.put(hash, data).await
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

    // -----------------------------------------------------------------------
    // Metadata mutation
    // -----------------------------------------------------------------------

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

/// UTC timestamp in RFC 3339 format.
pub(crate) fn chrono_now() -> Timestamp {
    Timestamp::new(baml_rt_core::now_unix_secs(clock_events::REPOSITORY_TIMESTAMP).to_string())
}

fn manifest_tags(source: &crate::entry::SourceBundle) -> Vec<Tag> {
    source
        .manifest
        .tags()
        .into_iter()
        .map(|tag| Tag::new(tag.to_string()))
        .collect()
}
