//! [`SurrealBackend`], [`SurrealStoreBuilder`], and process-wide store caching.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, OnceLock},
};

use surrealdb::{
    Surreal,
    engine::local::{Db, Mem, SurrealKv},
};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    SurrealProvenanceStore,
    helpers::map_surreal_error,
    schema::{self, init_schema},
};
use crate::{
    error::{ProvenanceError, Result},
    mermaid_cache::MermaidCache,
    normalizer::DefaultProvNormalizer,
};

// ---------------------------------------------------------------------------
// Backend enum + builder
// ---------------------------------------------------------------------------

/// Backend strategy for SurrealDB provenance store.
///
/// Storage backend strategy: file-backed (SurrealKV), in-memory shared, or in-memory isolated.
#[derive(Clone, Debug)]
pub enum SurrealBackend {
    /// File-backed: SurrealKV embedded storage in a directory.
    /// One shared store per directory path.
    File(crate::surreal_config::SurrealStoreConfig),
    /// In-memory shared: one global store for the process.
    InMemoryShared,
    /// Fresh isolated in-memory store per call (for tests).
    ///
    /// Each build selects a unique Surreal namespace/database so parallel test processes do not
    /// collide on the default `provenance`/`store` in-memory KV scope.
    InMemoryIsolated,
}

impl SurrealBackend {
    /// File-backed store at the given directory path.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self::File(crate::surreal_config::SurrealStoreConfig::file(
            path.as_ref(),
        ))
    }

    /// In-memory store shared by all callers.
    pub fn in_memory_shared() -> Self {
        Self::InMemoryShared
    }

    /// Build a store from this backend config.
    pub async fn build_store(
        &self,
        mermaid_cache: Option<Arc<MermaidCache>>,
    ) -> Result<Arc<SurrealProvenanceStore>> {
        match self {
            SurrealBackend::File(config) => {
                get_or_init_file_store(&config.path, mermaid_cache).await
            }
            SurrealBackend::InMemoryShared => {
                get_or_init_shared_in_memory_store(mermaid_cache).await
            }
            SurrealBackend::InMemoryIsolated => build_in_memory_isolated_store(mermaid_cache).await,
        }
    }
}

/// Builder for the SurrealDB provenance store.
pub struct SurrealStoreBuilder {
    backend: Option<SurrealBackend>,
    mermaid_cache: Option<Arc<MermaidCache>>,
}

impl SurrealStoreBuilder {
    pub fn new() -> Self {
        Self {
            backend: None,
            mermaid_cache: None,
        }
    }

    /// File-backed store at the given directory path.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self {
            backend: Some(SurrealBackend::file(path)),
            mermaid_cache: None,
        }
    }

    /// In-memory store shared by all callers.
    pub fn in_memory() -> Self {
        Self {
            backend: Some(SurrealBackend::in_memory_shared()),
            mermaid_cache: None,
        }
    }

    /// Fresh isolated in-memory store (for tests).
    pub fn in_memory_isolated() -> Self {
        Self {
            backend: Some(SurrealBackend::InMemoryIsolated),
            mermaid_cache: None,
        }
    }

    /// Use an explicit backend.
    pub fn backend(backend: SurrealBackend) -> Self {
        Self {
            backend: Some(backend),
            mermaid_cache: None,
        }
    }

    /// Attach Mermaid cache for invalidation on add_event.
    pub fn with_mermaid_cache(mut self, cache: Arc<MermaidCache>) -> Self {
        self.mermaid_cache = Some(cache);
        self
    }

    /// Build the store.
    pub async fn build(self) -> Result<Arc<SurrealProvenanceStore>> {
        let backend = self.backend.ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: String::new(),
            reason: "SurrealStoreBuilder: no backend set".to_string(),
        })?;
        backend.build_store(self.mermaid_cache).await
    }
}

impl Default for SurrealStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Store caching (shared/file)
// ---------------------------------------------------------------------------

/// File-backed stores cached by canonicalized path.
static FILE_STORES: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<SurrealProvenanceStore>>>> =
    OnceLock::new();

/// Shared in-memory singleton.
static SHARED_IN_MEMORY_STORE: OnceLock<Mutex<Option<Arc<SurrealProvenanceStore>>>> =
    OnceLock::new();

async fn get_or_init_file_store(
    path: &std::path::Path,
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let mutex = FILE_STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = mutex.lock().await;
    if let Some(store) = guard.get(path) {
        return Ok(store.clone());
    }
    let db = Surreal::new::<SurrealKv>(path.to_string_lossy().as_ref())
        .await
        .map_err(map_surreal_error)?;
    let store = init_store(db, mermaid_cache).await?;
    guard.insert(path.to_path_buf(), store.clone());
    Ok(store)
}

async fn get_or_init_shared_in_memory_store(
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let mutex = SHARED_IN_MEMORY_STORE.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().await;
    if let Some(store) = guard.as_ref() {
        return Ok(store.clone());
    }
    let store = build_in_memory_isolated_store(mermaid_cache).await?;
    *guard = Some(store.clone());
    Ok(store)
}

async fn build_in_memory_isolated_store(
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let db = Surreal::new::<Mem>(()).await.map_err(map_surreal_error)?;
    let scope = format!("isol_{}", Uuid::new_v4().simple());
    init_store_in_namespace(db, mermaid_cache, &scope, &scope).await
}

async fn init_store(
    db: Surreal<Db>,
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    init_store_in_namespace(db, mermaid_cache, schema::NS, schema::DB).await
}

async fn init_store_in_namespace(
    db: Surreal<Db>,
    mermaid_cache: Option<Arc<MermaidCache>>,
    ns: &str,
    db_name: &str,
) -> Result<Arc<SurrealProvenanceStore>> {
    db.use_ns(ns)
        .use_db(db_name)
        .await
        .map_err(map_surreal_error)?;
    init_schema(&db).await?;
    let store = SurrealProvenanceStore {
        db,
        normalizer: Arc::new(DefaultProvNormalizer::default()),
        mermaid_cache,
        task_agent_id_cache: dashmap::DashMap::new(),
    };
    Ok(Arc::new(store))
}
