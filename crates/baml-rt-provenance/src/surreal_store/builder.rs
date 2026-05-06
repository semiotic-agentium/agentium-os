//! [`SurrealBackend`], [`SurrealStoreBuilder`], and process-wide store caching.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, OnceLock},
};

use surrealdb::{Surreal, engine::any::Any};
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
    /// Remote SurrealDB server via WebSocket.
    Remote(RemoteConfig),
}

/// Connection config for a remote SurrealDB server.
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    /// WebSocket endpoint, e.g. `ws://surrealdb.agentium.svc:8000`.
    pub endpoint: String,
    /// SurrealDB namespace (defaults to `"provenance"`).
    pub namespace: String,
    /// SurrealDB database (defaults to `"store"`).
    pub database: String,
    /// Optional root credentials.
    pub credentials: Option<RemoteCredentials>,
}

/// Root credentials for signing into a remote SurrealDB server.
#[derive(Clone)]
pub struct RemoteCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for RemoteCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCredentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
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

    /// Remote SurrealDB via WebSocket with default namespace/database.
    pub fn remote(endpoint: impl Into<String>) -> Self {
        Self::Remote(RemoteConfig {
            endpoint: endpoint.into(),
            namespace: schema::NS.to_string(),
            database: schema::DB.to_string(),
            credentials: None,
        })
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
            SurrealBackend::Remote(config) => build_remote_store(config, mermaid_cache).await,
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
    let endpoint = format!("surrealkv://{}", path.to_string_lossy());
    let db = surrealdb::engine::any::connect(&endpoint)
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
    let db = surrealdb::engine::any::connect("mem://")
        .await
        .map_err(map_surreal_error)?;
    let store = init_store_in_namespace(db, mermaid_cache, schema::NS, schema::DB).await?;
    *guard = Some(store.clone());
    Ok(store)
}

async fn build_in_memory_isolated_store(
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let db = surrealdb::engine::any::connect("mem://")
        .await
        .map_err(map_surreal_error)?;
    let scope = format!("isol_{}", Uuid::new_v4().simple());
    init_store_in_namespace(db, mermaid_cache, &scope, &scope).await
}

/// Remote stores cached by (endpoint, namespace, database) composite key.
static REMOTE_STORES: OnceLock<Mutex<HashMap<String, Arc<SurrealProvenanceStore>>>> =
    OnceLock::new();

async fn build_remote_store(
    config: &RemoteConfig,
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    let cred_discriminator = match &config.credentials {
        Some(c) => {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(c.password.as_bytes());
            format!("@{}#{:x}", c.username, hash)
        }
        None => "@anon".to_string(),
    };
    let cache_key = format!(
        "{endpoint}|{namespace}|{database}|{cred_discriminator}",
        endpoint = config.endpoint,
        namespace = config.namespace,
        database = config.database,
    );
    let mutex = REMOTE_STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = mutex.lock().await;
    if let Some(store) = guard.get(&cache_key) {
        return Ok(store.clone());
    }

    let db = surrealdb::engine::any::connect(&config.endpoint)
        .await
        .map_err(map_surreal_error)?;
    if let Some(creds) = &config.credentials {
        db.signin(surrealdb::opt::auth::Root {
            username: creds.username.clone(),
            password: creds.password.clone(),
        })
        .await
        .map_err(map_surreal_error)?;
    }
    let store =
        init_store_in_namespace(db, mermaid_cache, &config.namespace, &config.database).await?;
    guard.insert(cache_key, store.clone());
    Ok(store)
}

async fn init_store(
    db: Surreal<Any>,
    mermaid_cache: Option<Arc<MermaidCache>>,
) -> Result<Arc<SurrealProvenanceStore>> {
    init_store_in_namespace(db, mermaid_cache, schema::NS, schema::DB).await
}

async fn init_store_in_namespace(
    db: Surreal<Any>,
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
        archive_prefix_cache: dashmap::DashMap::new(),
        archive_local_serializers: dashmap::DashMap::new(),
        archive_anchor_serializers: dashmap::DashMap::new(),
    };
    Ok(Arc::new(store))
}
