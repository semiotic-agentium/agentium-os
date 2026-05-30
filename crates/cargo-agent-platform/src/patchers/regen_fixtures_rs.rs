// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Patcher for baml-rt-builder/src/bin/regen_fixtures.rs to force-link new optional tools.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::Patcher;

pub struct RegenFixturesRsPatcher;

impl Patcher for RegenFixturesRsPatcher {
    fn file_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join("crates/baml-rt-builder/src/bin/regen_fixtures.rs")
    }

    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String> {
        let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));
        let use_line = format!(
            "use {crate_name} as _; // Force link so {tool_name} tool metadata is in inventory"
        );
        if content.contains(&format!("use {crate_name} as _;")) {
            return Ok(content.to_string());
        }

        let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();
        let insert_at = lines
            .iter()
            .position(|line| line.trim() == "use baml_tools_system as _; // Force link so system tool metadata is in inventory")
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find insertion point (use baml_tools_system as _;) in regen_fixtures.rs"
                )
            })?;

        lines.insert(insert_at, use_line);
        lines.insert(insert_at, format!("#[cfg(feature = \"{tool_name}\")]"));

        Ok(format!("{}\n", lines.join("\n")))
    }

    fn tool_exists(&self, content: &str, tool_name: &str) -> bool {
        let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));
        content.contains(&format!("use {crate_name} as _;"))
    }
}
