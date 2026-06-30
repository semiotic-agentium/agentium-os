// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use anyhow::{Result, bail};

/// Find the workspace root by looking for Cargo.toml with [workspace].
pub fn find_workspace_root() -> Result<PathBuf> {
    let mut current = std::env::current_dir()?;
    loop {
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = std::fs::read_to_string(&cargo_toml)?;
            if content.contains("[workspace]") {
                return Ok(current);
            }
        }

        if !current.pop() {
            bail!(
                "Error: could not find workspace root (Cargo.toml with [workspace] section).\nHint: run this command from the repository root or a subdirectory inside it."
            );
        }
    }
}
