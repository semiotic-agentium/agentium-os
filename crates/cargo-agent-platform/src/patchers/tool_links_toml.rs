//! Patcher for baml-tool-links/Cargo.toml to add new tool dependency and feature.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Formatted, InlineTable, Item, Value};

use super::Patcher;

pub struct ToolLinksTomlPatcher;

impl Patcher for ToolLinksTomlPatcher {
    fn file_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join("crates/baml-tool-links/Cargo.toml")
    }

    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String> {
        let mut doc: DocumentMut = content
            .parse()
            .context("Failed to parse baml-tool-links/Cargo.toml")?;

        let dep_name = format!("baml-tools-{tool_name}");
        let dep_path = format!("../tools/{tool_name}");

        // Add optional dependency
        let deps = doc
            .get_mut("dependencies")
            .and_then(|d| d.as_table_mut())
            .context("Missing [dependencies] section")?;

        if !deps.contains_key(&dep_name) {
            // Create inline table for the dependency: { path = "...", optional = true }
            let mut inline = InlineTable::new();
            inline.insert("path", Value::String(Formatted::new(dep_path)));
            inline.insert("optional", Value::Boolean(Formatted::new(true)));
            deps.insert(&dep_name, Item::Value(Value::InlineTable(inline)));
        }

        // Add feature
        let features = doc
            .get_mut("features")
            .and_then(|f| f.as_table_mut())
            .context("Missing [features] section")?;

        let feature_value = format!("dep:{dep_name}");
        if !features.contains_key(tool_name) {
            // Create array with single value
            let mut arr = toml_edit::Array::new();
            arr.push(&feature_value);
            features.insert(tool_name, Item::Value(Value::Array(arr)));
        }

        // Add to all-tools feature
        if let Some(Item::Value(Value::Array(all_tools))) = features.get_mut("all-tools")
            && !all_tools.iter().any(|v| v.as_str() == Some(tool_name))
        {
            // Insert in sorted position
            let mut items: Vec<String> = all_tools
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            items.push(tool_name.to_string());
            items.sort();

            all_tools.clear();
            for item in items {
                all_tools.push(&item);
            }
        }

        Ok(doc.to_string())
    }

    fn tool_exists(&self, content: &str, tool_name: &str) -> bool {
        let dep_name = format!("baml-tools-{tool_name}");
        content.contains(&dep_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_tool_links_toml() {
        let content = r#"[package]
name = "baml-tool-links"

[dependencies]
baml-tools-calculator = { path = "../tools/calculator" }
baml-tools-slack = { path = "../tools/slack", optional = true }

[features]
default = []
slack = ["dep:baml-tools-slack"]
all-tools = ["slack"]
"#;

        let patcher = ToolLinksTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        assert!(result.contains("baml-tools-github"));
        assert!(result.contains("github = [\"dep:baml-tools-github\"]"));
    }

    #[test]
    fn test_idempotent() {
        let content = r#"[package]
name = "baml-tool-links"

[dependencies]
baml-tools-github = { path = "../tools/github", optional = true }

[features]
github = ["dep:baml-tools-github"]
all-tools = ["github"]
"#;

        let patcher = ToolLinksTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        // Should not add duplicate
        assert_eq!(result.matches("baml-tools-github").count(), 2); // dependency + feature ref
    }
}
