// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Patcher for baml-rt-builder/Cargo.toml to add feature forwarding.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Formatted, InlineTable, Item, Value};

use super::Patcher;

pub struct BuilderTomlPatcher;

impl Patcher for BuilderTomlPatcher {
    fn file_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join("crates/baml-rt-builder/Cargo.toml")
    }

    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String> {
        let mut doc: DocumentMut = content
            .parse()
            .context("Failed to parse baml-rt-builder/Cargo.toml")?;

        let dep_name = format!("baml-tools-{tool_name}");
        let dep_path = format!("../tools/{tool_name}");
        let feature_forward = format!("dep:{dep_name}");

        // Ensure optional dependency exists in [dependencies]
        let deps = doc
            .get_mut("dependencies")
            .and_then(|d| d.as_table_mut())
            .context("Missing [dependencies] section in builder Cargo.toml")?;

        if !deps.contains_key(&dep_name) {
            let mut inline = InlineTable::new();
            inline.insert("path", Value::String(Formatted::new(dep_path)));
            inline.insert("optional", Value::Boolean(Formatted::new(true)));
            deps.insert(&dep_name, Item::Value(Value::InlineTable(inline)));
        }

        // Get or create features table
        let features = doc
            .get_mut("features")
            .and_then(|f| f.as_table_mut())
            .context("Missing [features] section in builder Cargo.toml")?;

        if !features.contains_key(tool_name) {
            // Create array with single forwarding value
            let mut arr = toml_edit::Array::new();
            arr.push(&feature_forward);
            features.insert(tool_name, Item::Value(Value::Array(arr)));
        }

        Ok(doc.to_string())
    }

    fn tool_exists(&self, content: &str, tool_name: &str) -> bool {
        // Check if feature exists in [features] section
        let feature_pattern = format!("{tool_name} = ");
        content.contains(&feature_pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_builder_toml() {
        let content = r#"[package]
name = "baml-rt-builder"

[dependencies]
baml-tools-slack = { path = "../tools/slack", optional = true }

[features]
default = []
slack = ["dep:baml-tools-slack"]
"#;

        let patcher = BuilderTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        assert!(
            result.contains("baml-tools-github = { path = \"../tools/github\", optional = true }")
        );
        assert!(result.contains("github = [\"dep:baml-tools-github\"]"));
    }

    #[test]
    fn test_idempotent() {
        let content = r#"[package]
name = "baml-rt-builder"

[dependencies]
baml-tools-github = { path = "../tools/github", optional = true }

[features]
github = ["dep:baml-tools-github"]
"#;

        let patcher = BuilderTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        // Should not add duplicate feature line or dependency line
        assert_eq!(
            result
                .matches("github = [\"dep:baml-tools-github\"]")
                .count(),
            1
        );
        assert_eq!(
            result
                .matches("baml-tools-github = { path = \"../tools/github\", optional = true }")
                .count(),
            1
        );
    }
}
