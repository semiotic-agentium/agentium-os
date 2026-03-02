//! Strong-typed configuration for the GraphQLite-backed provenance store.
//!
//! Construct only via named constructors so invalid states (e.g. empty path
//! when file-backed) are unrepresentable.

use std::{path::{Path, PathBuf}, sync::Arc};

use crate::mermaid_cache::MermaidCache;

/// Database location: either a file path or in-memory.
///
/// Use [StorePath::file] or [StorePath::in_memory]; do not construct directly
/// so path semantics stay explicit. Per SQLite: each `:memory:` connection is
/// a private DB; sharing one graph in memory means sharing one connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorePath {
    /// One DB file; multiple connections to the same path are allowed (SQLite locking).
    File(PathBuf),
    /// In-memory DB (`:memory:`); one connection = one private DB.
    InMemory,
}

impl StorePath {
    /// File-backed store at the given path. Shared graph for all agents.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self::File(path.as_ref().to_path_buf())
    }

    /// In-memory store. One connection = one private DB (SQLite semantics).
    pub fn in_memory() -> Self {
        Self::InMemory
    }

    /// Path for file-backed stores; `None` for in-memory. Used as cache key for shared store per path.
    pub fn file_path(&self) -> Option<PathBuf> {
        match self {
            Self::File(p) => Some(p.clone()),
            Self::InMemory => None,
        }
    }

    /// String suitable for opening a connection (path or `:memory:`).
    pub fn as_connection_str(&self) -> String {
        match self {
            Self::File(p) => p
                .to_str()
                .map(str::to_string)
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            Self::InMemory => ":memory:".to_string(),
        }
    }
}

/// Configuration for the GraphQLite store. Build via [GraphqliteStoreConfig::file]
/// or [GraphqliteStoreConfig::in_memory].
#[derive(Clone, Debug)]
pub struct GraphqliteStoreConfig {
    /// Where the DB lives (file or in-memory). Per SQLite: file = multiple connections OK; :memory: = one connection per DB.
    pub path: StorePath,
    /// Use WAL so multiple agents can write without blocking each other.
    pub wal: bool,
    /// Optional Mermaid cache for context-scoped diagram invalidation on add_event.
    pub mermaid_cache: Option<Arc<MermaidCache>>,
}

impl GraphqliteStoreConfig {
    /// File-backed store at the given path. WAL enabled by default.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self {
            path: StorePath::file(path),
            wal: true,
            mermaid_cache: None,
        }
    }

    /// In-memory store. One connection = one private DB (SQLite semantics).
    pub fn in_memory() -> Self {
        Self {
            path: StorePath::in_memory(),
            wal: true,
            mermaid_cache: None,
        }
    }

    /// Attach a Mermaid cache for invalidation on add_event. File-backed only.
    pub fn with_mermaid_cache(mut self, cache: Arc<MermaidCache>) -> Self {
        self.mermaid_cache = Some(cache);
        self
    }
}
