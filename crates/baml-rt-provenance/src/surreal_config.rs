//! Configuration types for the SurrealDB provenance backend.
//!
//! Mirrors the ergonomics of [`GraphqliteStoreConfig`](crate::graphqlite_config::GraphqliteStoreConfig)
//! while respecting SurrealDB's different storage semantics:
//!
//! - **File mode** uses a directory path (SurrealKV embedded storage), not a single `.db` file.
//! - **In-memory modes** use `mem://` internally; shared vs isolated is handled by the store builder.
//!
//! Callers (runner, tests) choose the backend mode; this crate does not decide from feature flags.

use std::path::PathBuf;

/// Configuration for a file-backed SurrealDB store.
///
/// The `path` is a **directory** (SurrealKV stores data across multiple files within a directory).
/// This differs from GraphQLite which uses a single `.db` file.
#[derive(Clone, Debug)]
pub struct SurrealStoreConfig {
    /// Directory path for SurrealKV embedded storage.
    pub path: PathBuf,
}

impl SurrealStoreConfig {
    /// Create a file-backed config with the given directory path.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}
