// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Patcher for baml-agent-runner/src/main.rs to force-link new optional tools.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::Patcher;

pub struct RunnerMainRsPatcher;

impl Patcher for RunnerMainRsPatcher {
    fn file_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join("crates/baml-agent-runner/src/main.rs")
    }

    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String> {
        let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));
        let use_line = format!("use {crate_name} as _;");
        if content.contains(&use_line) {
            return Ok(content.to_string());
        }

        let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();
        let insert_at = lines
            .iter()
            .position(|line| line.trim() == "use baml_tools_system::SystemBundle;")
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find insertion point (use baml_tools_system::SystemBundle;) in runner main.rs"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_cfg_use_before_system_bundle_import() {
        let content = r#"
use baml_tools_calculator as _;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use baml_tools_system::SystemBundle;
"#;

        let patcher = RunnerMainRsPatcher;
        let patched = patcher.patch_for_tool(content, "jira").expect("patch");
        assert!(patched.contains("#[cfg(feature = \"jira\")]"));
        assert!(patched.contains("use baml_tools_jira as _;"));
    }
}
