//! `sandbox-digest` subcommand implementation.
//!
//! Computes runtime content digests for sandbox image sources.

use std::path::Path;

use anyhow::{Result, bail};
use baml_rt_tools::external_tools::sandbox_runtime_digest_for_bind;
use clap::ValueEnum;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum SandboxDigestSourceArg {
    Bind,
}

pub fn run(source: SandboxDigestSourceArg, path: &str) -> Result<()> {
    let path = Path::new(path);

    match source {
        SandboxDigestSourceArg::Bind => {
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| anyhow::anyhow!("bind path does not resolve: {}: {e}", path.display()))?;
            if !canonical.is_dir() {
                bail!("bind path is not a directory: {}", canonical.display());
            }
            let digest = sandbox_runtime_digest_for_bind(&canonical)?;
            println!("{digest}");
        }
    }

    Ok(())
}
