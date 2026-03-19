//! Patcher for baml-rt-builder/Cargo.toml to add feature forwarding.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Value};

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

        let feature_forward = format!("baml-tool-links/{tool_name}");

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
baml-tool-links = { path = "../baml-tool-links" }

[features]
default = []
slack = ["baml-tool-links/slack"]
"#;

        let patcher = BuilderTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        assert!(result.contains("github = [\"baml-tool-links/github\"]"));
    }

    #[test]
    fn test_idempotent() {
        let content = r#"[package]
name = "baml-rt-builder"

[features]
github = ["baml-tool-links/github"]
"#;

        let patcher = BuilderTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        // Should not add duplicate
        assert_eq!(result.matches("github =").count(), 1);
    }
}
