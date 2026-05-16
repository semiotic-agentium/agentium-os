//! Atomic file writes for generated artifacts.

use std::path::Path;

use crate::builder::error::Result;

/// Re-export of [`baml_rt_core::atomic_io::atomic_write`] as a builder-local name so
/// callers in `tsc.rs` and `runtime_type_gen.rs` keep importing
/// `use super::atomic_io::atomic_write` unchanged. The `?` at each call site converts
/// the returned `io::Error` to `BamlBuilderError` via the existing `From` impl.
pub(crate) fn atomic_write(dest: &Path, data: &[u8]) -> Result<()> {
    baml_rt_core::atomic_io::atomic_write(dest, data)?;
    Ok(())
}
