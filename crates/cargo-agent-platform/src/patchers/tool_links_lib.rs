//! Patcher for baml-tool-links/src/lib.rs to add tool to force_link_all_tools! macro.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::Patcher;

pub struct ToolLinksLibPatcher;

impl Patcher for ToolLinksLibPatcher {
    fn file_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join("crates/baml-tool-links/src/lib.rs")
    }

    fn patch_for_tool(&self, content: &str, tool_name: &str) -> Result<String> {
        let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));

        // Check if already present
        if content.contains(&crate_name) {
            return Ok(content.to_string());
        }

        let use_line = format!(
            "        #[cfg(feature = \"{tool_name}\")]\n        use $crate::{crate_name} as _;"
        );
        let reexport_line = format!("#[cfg(feature = \"{tool_name}\")]\npub use {crate_name};");

        let mut result = content.to_string();

        // 1. Add re-export near the top of the file
        // Find the last feature-gated "pub use" line and insert after it
        if let Some(insert_pos) = find_last_feature_gated_reexport(&result) {
            result.insert_str(insert_pos, &format!("\n{reexport_line}"));
        } else if let Some(insert_pos) = find_last_unconditional_reexport(&result) {
            // No feature-gated re-exports, insert after unconditional ones
            result.insert_str(insert_pos, &format!("\n{reexport_line}"));
        }

        // 2. Add use line inside the force_link_all_tools! macro
        // Find the last feature-gated use inside the macro
        if let Some(insert_pos) = find_macro_insertion_point(&result) {
            result.insert_str(insert_pos, &format!("\n{use_line}"));
        } else {
            bail!("Could not find insertion point in force_link_all_tools! macro");
        }

        Ok(result)
    }

    fn tool_exists(&self, content: &str, tool_name: &str) -> bool {
        let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));
        content.contains(&crate_name)
    }
}

/// Find the end position of the last feature-gated `pub use` re-export.
fn find_last_feature_gated_reexport(content: &str) -> Option<usize> {
    // Look for patterns like:
    // #[cfg(feature = "...")]
    // pub use ...;
    let mut last_end = None;

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("#[cfg(feature =") && line.ends_with(")]") && i + 1 < lines.len() {
            let next_line = lines[i + 1].trim();
            if next_line.starts_with("pub use ") && next_line.ends_with(';') {
                // Calculate the byte position of the end of this statement
                let mut pos = 0;
                for (j, line) in lines.iter().enumerate() {
                    if j <= i + 1 {
                        pos += line.len() + 1; // +1 for newline
                    }
                }
                last_end = Some(pos - 1); // -1 to position at end of line
                i += 2;
                continue;
            }
        }
        i += 1;
    }

    last_end
}

/// Find the end position of the last unconditional `pub use` re-export.
fn find_last_unconditional_reexport(content: &str) -> Option<usize> {
    let mut last_end = None;

    let lines: Vec<&str> = content.lines().collect();
    let mut pos = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // Check if it's an unconditional pub use (not preceded by #[cfg])
        if trimmed.starts_with("pub use ") && trimmed.ends_with(';') {
            // Make sure previous line is not a #[cfg(...)]
            let is_conditional = i > 0 && lines[i - 1].trim().starts_with("#[cfg(");
            if !is_conditional {
                last_end = Some(pos + line.len());
            }
        }
        pos += line.len() + 1; // +1 for newline
    }

    last_end
}

/// Find the insertion point inside the force_link_all_tools! macro.
///
/// We want to insert after the last feature-gated use line inside the macro.
fn find_macro_insertion_point(content: &str) -> Option<usize> {
    // Find the macro definition
    let macro_start = content.find("macro_rules! force_link_all_tools")?;
    let macro_content = &content[macro_start..];

    // Find the last #[cfg(feature = "...")] use $crate::... as _; pattern inside the macro
    let lines: Vec<&str> = macro_content.lines().collect();
    let mut last_use_end = None;
    let mut pos = macro_start;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Look for feature-gated use inside macro
        if trimmed.starts_with("#[cfg(feature =") && i + 1 < lines.len() {
            let next_trimmed = lines[i + 1].trim();
            if next_trimmed.starts_with("use $crate::") && next_trimmed.ends_with(" as _;") {
                // Calculate position at end of the use line
                pos += line.len() + 1 + lines[i + 1].len();
                last_use_end = Some(pos);
                i += 2;
                continue;
            }
        }

        pos += line.len() + 1;
        i += 1;
    }

    last_use_end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_exists() {
        let content = r#"
#[cfg(feature = "slack")]
pub use baml_tools_slack;

#[macro_export]
macro_rules! force_link_all_tools {
    () => {
        #[cfg(feature = "slack")]
        use $crate::baml_tools_slack as _;
    };
}
"#;

        let patcher = ToolLinksLibPatcher;
        assert!(patcher.tool_exists(content, "slack"));
        assert!(!patcher.tool_exists(content, "github"));
    }

    #[test]
    fn test_find_macro_insertion_point() {
        let content = r#"
#[macro_export]
macro_rules! force_link_all_tools {
    () => {
        #[cfg(feature = "clickup")]
        use $crate::baml_tools_clickup as _;
        #[cfg(feature = "slack")]
        use $crate::baml_tools_slack as _;
        use $crate::baml_tools_calculator as _;
    };
}
"#;

        let pos = find_macro_insertion_point(content);
        assert!(pos.is_some());
    }
}
