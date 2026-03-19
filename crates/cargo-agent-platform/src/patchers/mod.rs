//! File patchers for workspace configuration files.
//!
//! Each patcher knows how to modify a specific configuration file
//! to add a new tool crate.

pub mod builder_toml;
pub mod runner_toml;
pub mod tool_links_lib;
pub mod tool_links_toml;
pub mod workspace_toml;

use std::path::Path;

use anyhow::Result;

/// Trait for configuration file patchers.
pub trait Patcher {
    /// Get the path to the file this patcher modifies.
    fn file_path(&self, workspace_root: &Path) -> std::path::PathBuf;

    /// Apply the patch for a new tool.
    ///
    /// Returns the modified file content.
    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String>;

    /// Check if the tool is already present in the file.
    fn tool_exists(&self, content: &str, tool_name: &str) -> bool;
}
