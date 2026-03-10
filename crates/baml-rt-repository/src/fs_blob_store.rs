//! Filesystem-backed blob store.
//!
//! Stores tar.gz packages as files under a content-addressable directory tree:
//! `<root>/ab/cdef01...` (first 2 hex chars as shard directory, rest as filename).
//! This prevents any single directory from growing too large.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::{RepositoryError, Result};
use crate::ids::ContentHash;
use crate::storage::BlobStore;

/// Filesystem-backed content-addressable blob store.
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    /// Create a new blob store rooted at `root`. Creates the directory if absent.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?;
        Ok(Self { root })
    }

    /// Derive the filesystem path for a given hash.
    /// Layout: `<root>/<first-2-hex>/<remaining-62-hex>.tar.gz`
    fn blob_path(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash.as_str();
        let (shard, rest) = hex.split_at(2);
        self.root.join(shard).join(format!("{rest}.tar.gz"))
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        let path = self.blob_path(hash);
        let data = data.to_vec();
        let hash_display = hash.to_string();
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| RepositoryError::StorageWrite {
                    source: Box::new(e),
                })?;
            }
            std::fs::write(&path, &data).map_err(|e| RepositoryError::StorageWrite {
                source: Box::new(e),
            })?;
            tracing::debug!(hash = %hash_display, path = %path.display(), event = "blob_written");
            Ok(())
        })
        .await
        .map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?
    }

    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let path = self.blob_path(hash);
        tokio::task::spawn_blocking(move || match std::fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(RepositoryError::StorageRead {
                source: Box::new(e),
            }),
        })
        .await
        .map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.blob_path(hash);
        tokio::task::spawn_blocking(move || Ok(path.exists()))
            .await
            .map_err(|e| RepositoryError::StorageRead {
                source: Box::new(e),
            })?
    }

    async fn delete(&self, hash: &ContentHash) -> Result<()> {
        let path = self.blob_path(hash);
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(RepositoryError::StorageWrite {
                source: Box::new(e),
            }),
        })
        .await
        .map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsBlobStore::new(dir.path()).unwrap();

        let hash: ContentHash = "a".repeat(64).parse().unwrap();
        let data = b"fake tar.gz content";

        assert!(!store.exists(&hash).await.unwrap());
        assert!(store.get(&hash).await.unwrap().is_none());

        store.put(&hash, data).await.unwrap();
        assert!(store.exists(&hash).await.unwrap());

        let read = store.get(&hash).await.unwrap().unwrap();
        assert_eq!(read, data);

        store.delete(&hash).await.unwrap();
        assert!(!store.exists(&hash).await.unwrap());
        store.delete(&hash).await.unwrap();
    }

    #[tokio::test]
    async fn shard_directory_created() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsBlobStore::new(dir.path()).unwrap();

        let hash: ContentHash = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
            .parse()
            .unwrap();
        store.put(&hash, b"data").await.unwrap();
        assert!(dir.path().join("ab").is_dir());
    }
}
