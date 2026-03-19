//! Patcher for workspace Cargo.toml to add new tool crate to members.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::DocumentMut;

use super::Patcher;

pub struct WorkspaceTomlPatcher;

impl Patcher for WorkspaceTomlPatcher {
    fn file_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join("Cargo.toml")
    }

    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String> {
        let mut doc: DocumentMut = content
            .parse()
            .context("Failed to parse workspace Cargo.toml")?;

        let member_path = format!("crates/tools/{tool_name}");

        // Get or create workspace.members array
        let workspace = doc
            .get_mut("workspace")
            .and_then(|w| w.as_table_mut())
            .context("Missing [workspace] section in Cargo.toml")?;

        let members = workspace
            .get_mut("members")
            .and_then(|m| m.as_array_mut())
            .context("Missing workspace.members array")?;

        // Check if already present
        if members.iter().any(|v| v.as_str() == Some(&member_path)) {
            return Ok(content.to_string());
        }

        // Find insertion point (sorted among crates/tools/ entries)
        let mut insert_idx = None;
        for (i, item) in members.iter().enumerate() {
            if let Some(s) = item.as_str()
                && s.starts_with("crates/tools/")
            {
                if s > member_path.as_str() {
                    insert_idx = Some(i);
                    break;
                }
                // Update insert_idx to after the last crates/tools/ entry
                insert_idx = Some(i + 1);
            }
        }

        // If no crates/tools/ entries found, insert at the end
        let insert_idx = insert_idx.unwrap_or(members.len());

        // Insert the new member
        members.insert(insert_idx, &member_path);

        Ok(doc.to_string())
    }

    fn tool_exists(&self, content: &str, tool_name: &str) -> bool {
        let member_path = format!("crates/tools/{tool_name}");
        content.contains(&member_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_workspace_toml() {
        let content = r#"[workspace]
members = [
  "crates/baml-rt-core",
  "crates/tools/calculator",
  "crates/tools/slack",
]
"#;

        let patcher = WorkspaceTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        // github should be inserted between calculator and slack (alphabetically)
        assert!(result.contains("crates/tools/github"));

        // Verify order by checking positions
        let calc_pos = result.find("crates/tools/calculator").unwrap();
        let github_pos = result.find("crates/tools/github").unwrap();
        let slack_pos = result.find("crates/tools/slack").unwrap();

        assert!(calc_pos < github_pos);
        assert!(github_pos < slack_pos);
    }

    #[test]
    fn test_idempotent() {
        let content = r#"[workspace]
members = [
  "crates/tools/github",
]
"#;

        let patcher = WorkspaceTomlPatcher;
        let result = patcher.patch_for_tool(content, "github").unwrap();

        // Should not add duplicate
        assert_eq!(result.matches("crates/tools/github").count(), 1);
    }

    #[test]
    fn test_tool_exists() {
        let content = r#"[workspace]
members = [
  "crates/tools/github",
]
"#;

        let patcher = WorkspaceTomlPatcher;
        assert!(patcher.tool_exists(content, "github"));
        assert!(!patcher.tool_exists(content, "jira"));
    }
}
