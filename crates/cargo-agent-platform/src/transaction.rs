//! Transactional file writer with rollback support.
//!
//! Collects all file operations in memory, validates them, then writes
//! all at once. On failure, previously written files are restored from
//! their in-memory backups.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

/// A pending file write operation.
#[derive(Debug, Clone)]
pub struct PendingWrite {
    /// Path to write to
    pub path: PathBuf,
    /// Content to write
    pub content: Vec<u8>,
    /// Whether this is a new file (vs editing existing)
    pub is_new: bool,
}

/// Transactional writer that collects all file operations and applies them atomically.
///
/// If any write fails, all previously written files are restored from their snapshots.
#[derive(Default)]
pub struct TransactionalWriter {
    /// Snapshots of original file content (None if file didn't exist)
    snapshots: HashMap<PathBuf, Option<Vec<u8>>>,
    /// Pending write operations
    pending: Vec<PendingWrite>,
    /// Directories that were created (for cleanup on rollback)
    created_dirs: Vec<PathBuf>,
    /// Whether the transaction has been committed
    committed: bool,
}

impl TransactionalWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage an edit to an existing file.
    ///
    /// The file must exist. Its current content is snapshotted for rollback.
    pub fn stage_edit(
        &mut self,
        path: impl AsRef<Path>,
        content: impl Into<Vec<u8>>,
    ) -> Result<()> {
        let path = path.as_ref().to_path_buf();
        let content = content.into();

        // Snapshot the original content if not already done
        if !self.snapshots.contains_key(&path) {
            let original = fs::read(&path).with_context(|| {
                format!(
                    "Failed to read existing file for snapshot: {}",
                    path.display()
                )
            })?;
            self.snapshots.insert(path.clone(), Some(original));
        }

        self.pending.push(PendingWrite {
            path,
            content,
            is_new: false,
        });

        Ok(())
    }

    /// Stage creation of a new file.
    ///
    /// The parent directory must exist or will be created.
    pub fn stage_create(
        &mut self,
        path: impl AsRef<Path>,
        content: impl Into<Vec<u8>>,
    ) -> Result<()> {
        let path = path.as_ref().to_path_buf();
        let content = content.into();

        // Record that this file didn't exist (for deletion on rollback)
        if !self.snapshots.contains_key(&path) {
            self.snapshots.insert(path.clone(), None);
        }

        self.pending.push(PendingWrite {
            path,
            content,
            is_new: true,
        });

        Ok(())
    }

    /// Stage creation of a directory.
    ///
    /// Directories are created during commit and removed on rollback.
    pub fn stage_mkdir(&mut self, path: impl AsRef<Path>) {
        self.created_dirs.push(path.as_ref().to_path_buf());
    }

    /// Commit all pending operations.
    ///
    /// Creates directories first, then writes all files. On any failure,
    /// rolls back all changes and returns the error.
    pub fn commit(mut self) -> Result<()> {
        // Track which files we've actually written (for partial rollback)
        let mut written: Vec<PathBuf> = Vec::new();
        let mut dirs_created: Vec<PathBuf> = Vec::new();

        // Create directories first
        for dir in &self.created_dirs {
            if !dir.exists() {
                fs::create_dir_all(dir)
                    .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
                dirs_created.push(dir.clone());
            }
        }

        // Write all files
        for write in &self.pending {
            // Ensure parent directory exists
            if let Some(parent) = write.path.parent()
                && !parent.exists()
            {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create parent directory: {}", parent.display())
                })?;
                dirs_created.push(parent.to_path_buf());
            }

            // Write the file
            if let Err(e) = fs::write(&write.path, &write.content) {
                // Rollback on failure
                self.rollback_files(&written, &dirs_created);
                bail!("Failed to write {}: {}", write.path.display(), e);
            }

            written.push(write.path.clone());
        }

        self.committed = true;
        Ok(())
    }

    /// Rollback written files and created directories.
    fn rollback_files(&self, written: &[PathBuf], dirs_created: &[PathBuf]) {
        // Restore or delete files
        for path in written {
            if let Some(snapshot) = self.snapshots.get(path) {
                match snapshot {
                    Some(original) => {
                        // Restore original content
                        let _ = fs::write(path, original);
                    }
                    None => {
                        // File was new, delete it
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }

        // Remove created directories (in reverse order, deepest first)
        for dir in dirs_created.iter().rev() {
            // Only remove if empty (to avoid deleting user files)
            let _ = fs::remove_dir(dir);
        }
    }

    /// Discard the transaction without committing (for dry-run mode).
    ///
    /// This marks the transaction as committed (to suppress the Drop warning)
    /// without actually writing any files.
    pub fn discard(mut self) {
        self.committed = true;
    }

    /// Get a summary of pending operations (for dry-run mode).
    pub fn summary(&self) -> Vec<String> {
        let mut lines = Vec::new();

        for dir in &self.created_dirs {
            lines.push(format!("CREATE DIR  {}", dir.display()));
        }

        for write in &self.pending {
            let action = if write.is_new { "CREATE" } else { "EDIT" };
            lines.push(format!("{:<11} {}", action, write.path.display()));
        }

        lines
    }
}

impl Drop for TransactionalWriter {
    fn drop(&mut self) {
        if !self.committed && !self.pending.is_empty() {
            // Transaction was dropped without commit - this shouldn't happen in normal use
            // but we don't rollback here since nothing was written
            eprintln!("Warning: TransactionalWriter dropped without commit");
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_stage_create_and_commit() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");

        let mut writer = TransactionalWriter::new();
        writer.stage_create(&file_path, b"hello".to_vec()).unwrap();
        writer.commit().unwrap();

        assert!(file_path.exists());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "hello");
    }

    #[test]
    fn test_stage_edit_and_commit() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        fs::write(&file_path, "original").unwrap();

        let mut writer = TransactionalWriter::new();
        writer.stage_edit(&file_path, b"modified".to_vec()).unwrap();
        writer.commit().unwrap();

        assert_eq!(fs::read_to_string(&file_path).unwrap(), "modified");
    }

    #[test]
    fn test_summary() {
        let mut writer = TransactionalWriter::new();
        writer.stage_mkdir("/tmp/test");
        writer.pending.push(PendingWrite {
            path: PathBuf::from("/tmp/test/file.txt"),
            content: vec![],
            is_new: true,
        });

        let summary = writer.summary();
        assert_eq!(summary.len(), 2);
        assert!(summary[0].contains("CREATE DIR"));
        assert!(summary[1].contains("CREATE"));
    }
}
