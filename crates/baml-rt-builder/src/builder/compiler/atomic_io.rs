//! Atomic file writes for generated artifacts.

use std::{io::Write, path::Path};

use crate::builder::error::{BamlBuilderError, Result};

/// Write `data` to a temporary file in the same directory, then atomically rename
/// over `dest`.  On Unix `rename(2)` is atomic, so concurrent readers never see
/// a half-written file — they get either the old content or the new content.
pub(crate) fn atomic_write(dest: &Path, data: &[u8]) -> Result<()> {
    let parent = dest.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.persist(dest)
        .map_err(|e| BamlBuilderError::Io(e.error))?;
    Ok(())
}
