// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Atomic filesystem writes via temp-file-and-rename.

use std::{io::Write, path::Path};

/// Write `data` to `dest` atomically: stage in a temp file inside the same directory,
/// then rename over the destination.
///
/// Cross-platform via `tempfile::NamedTempFile::persist`, which uses the platform's
/// atomic-replace primitive (`rename(2)` on Unix; `ReplaceFileW`/`MoveFileExW` on
/// Windows). Concurrent readers either observe the prior contents or the new contents,
/// never a half-written file. The temp file is created in `dest.parent()` so the rename
/// stays within a single filesystem — cross-filesystem ranges would degrade to copy + delete
/// and lose atomicity.
pub fn atomic_write(dest: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent = dest.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}
