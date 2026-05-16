//! Atomic file writes for generated artifacts.

use std::path::Path;

use crate::builder::error::Result;

/// Wrapper over [`baml_rt_core::atomic_io::atomic_write`] that maps the I/O error into
/// `BamlBuilderError`. Kept as a thin crate-local helper so the builder modules stay
/// on `Result<T, BamlBuilderError>` without sprinkling `From<io::Error>` calls.
pub(crate) fn atomic_write(dest: &Path, data: &[u8]) -> Result<()> {
    baml_rt_core::atomic_io::atomic_write(dest, data)?;
    Ok(())
}
