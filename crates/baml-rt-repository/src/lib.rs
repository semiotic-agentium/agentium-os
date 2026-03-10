//! Agent package repository: content-addressable archive with lineage,
//! versioning, and search.
//!
//! The repository is a standalone service (local or cloud) that stores agent
//! packages as immutable, content-addressed entries. It provides:
//!
//! - **Persistence**: hybrid FS (tar.gz blobs) + SQLite (metadata, lineage).
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
//! - **BlobStore** (filesystem): tar.gz packages keyed by `ContentHash`.
//! - **MetadataStore** (SQLite): entries, version mappings, fitness scores, tags.
//! - **LineageStore** (SQLite): DAG edges, ancestry traversal.
//! - **SearchStore** (SQLite FTS5): full-text + metadata search.
//!
//! ## Canonical hash
//!
//! `ContentHash = SHA-256(manifest.json || sorted .ts || sorted .baml)`.
//! Runtime-generated artefacts (d.ts, tsconfig, compiled JS, baml_client/)
//! are excluded. Two packages with identical authored source produce
//! identical hashes.

// --- Domain types ---
pub mod ids;
pub mod lineage;
pub mod entry;
pub mod search;

// --- Operations ---
pub mod commands;
pub mod error;

// --- Storage trait boundaries ---
pub mod storage;

// --- Implementations ---
pub mod fs_blob_store;
pub mod sqlite_store;
pub mod service;

// --- HTTP API surface ---
pub mod http;
pub mod handlers;
pub mod router;

// --- Observability (orthogonal) ---
#[allow(dead_code)]
mod spans;
#[allow(dead_code)]
mod metrics;

// --- Re-exports for public API ---
pub use error::{RepositoryError, Result};
pub use ids::{AgentName, ContentHash, Generation, Version, VersionRef};
pub use entry::{ChangeRationale, RepositoryEntry, RepositoryEntryHeader, SourceBundle};
pub use lineage::{LineageEdge, LineageKind, LineageSubgraph, Parentage};
pub use commands::{ForkCommand, PublishCommand, PublishResult};
pub use storage::{BlobStore, LineageStore, MetadataStore, SearchStore};
pub use fs_blob_store::FsBlobStore;
pub use sqlite_store::SqliteStore;
pub use service::RepositoryService;
pub use router::repository_router;
