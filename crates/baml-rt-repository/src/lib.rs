// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent package repository: content-addressable archive with lineage,
//! versioning, and search.
//!
//! The repository is a standalone service (local or cloud) that stores agent
//! packages as immutable, content-addressed entries. It provides:
//!
//! - **Persistence**: SurrealDB embedded for blobs + metadata + lineage.
//! - **Versioning**: monotonic per-lineage versions with canonical hashing.
//! - **Lineage**: first-class fork and influence edges forming a DAG.
//! - **Search**: metadata filters + full-text over source content.
//! - **Distribution**: pull by hash or name@version.
//!
//! The repository is agnostic about *who* publishes or queries — human
//! developers, CI pipelines, or ADAS meta-agents all use the same API.
//!
//! ## Storage architecture
//!
//! A single embedded SurrealDB backend implements all storage traits.
//!
//! ## Canonical hash
//!
//! `ContentHash = SHA-256(manifest.json || sorted .ts || sorted .baml)`.
//! Runtime-generated artefacts (d.ts, tsconfig, compiled JS, baml_client/)
//! are excluded. Two packages with identical authored source produce
//! identical hashes.

// --- Domain types ---
pub mod entry;
pub mod external_tool;
pub mod ids;
pub mod lineage;
pub mod mcp;
pub mod search;

// --- Operations ---
pub mod commands;
pub mod dev_artifacts;
pub mod error;
pub mod identity;
pub mod package;

// --- Storage trait boundaries ---
pub mod storage;

// --- Implementations ---
pub mod service;
pub mod surreal_store;

// --- HTTP API surface ---
pub mod handlers;
pub mod http;
pub mod router;

// --- Observability (orthogonal) ---
#[expect(
    dead_code,
    reason = "observability scaffolding not yet wired into the repository surface"
)]
mod metrics;
#[expect(
    dead_code,
    reason = "observability scaffolding not yet wired into the repository surface"
)]
mod spans;

// --- Re-exports for public API ---
pub use baml_rt_hash::AgentIdentityDigest;
pub use commands::{DraftResult, ForkCommand, PublishCommand, PublishResult};
pub use dev_artifacts::{DevArtifactsBundle, dev_artifacts_blob_hash, resolve_package_hash};
pub use entry::{
    ChangeRationale, NewEntry, RepositoryEntry, RepositoryEntryHeader, SourceBundle,
    VersionReservation,
};
pub use error::{RepositoryError, Result};
pub use external_tool::{
    ExternalToolRegistryTool, ExternalToolRegistryToolVersion, ExternalToolSnapshotBlob,
    ExternalToolsetApprovalRecord,
};
pub use identity::{
    PackageIdentityError, artifact_hash_from_tar_gz, identity_digest_from_tar_gz,
    identity_entries_from_extracted_package, identity_entries_from_tar_gz,
};
pub use ids::{AgentName, ArtifactHash, ContentHash, Generation, Version, VersionRef};
pub use lineage::{LineageEdge, LineageKind, LineageSubgraph, Parentage};
pub use mcp::{
    McpRegistryServer, McpRegistryServerVersion, McpRegistryToolVersion, McpSnapshotBlob,
    compute_snapshot_digest,
};
pub use package::{
    PackageExtractError, manifest_package_name_from_tar_gz, source_bundle_from_agent_dir,
    source_bundle_from_tar_gz,
};
pub use router::{
    repository_mutation_router, repository_read_router, repository_router,
    repository_router_without_publish,
};
pub use service::{DEFAULT_VERSION_RESERVATION_TTL_MS, RepositoryService};
pub use storage::{
    BlobStore, ExternalToolRegistryStore, LineageStore, McpRegistryStore, MetadataStore,
    SearchStore,
};
pub use surreal_store::SurrealStore;
