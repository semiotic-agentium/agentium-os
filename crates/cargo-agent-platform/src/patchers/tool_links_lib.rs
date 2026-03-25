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

        let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();
        let reexport_cfg = format!("#[cfg(feature = \"{tool_name}\")]");
        let reexport_use = format!("pub use {crate_name};");
        let macro_cfg = format!("        #[cfg(feature = \"{tool_name}\")]");
        let macro_use = format!("        use $crate::{crate_name} as _;");

        // 1. Add feature-gated re-export line pair near existing re-exports.
        if let Some(idx) = find_reexport_insertion_index(&lines) {
            lines.insert(idx, reexport_cfg);
            lines.insert(idx + 1, reexport_use);
        } else {
            bail!("Could not find insertion point for re-export in baml-tool-links/src/lib.rs");
        }

        // 2. Add feature-gated force-link macro use lines inside force_link_all_tools!.
        if let Some(idx) = find_macro_cfg_use_insertion_index(&lines) {
            lines.insert(idx, macro_cfg);
            lines.insert(idx + 1, macro_use);
        } else {
            bail!("Could not find insertion point in force_link_all_tools! macro");
        }

        Ok(format!("{}\n", lines.join("\n")))
    }

    fn tool_exists(&self, content: &str, tool_name: &str) -> bool {
        let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));
        content.contains(&crate_name)
    }
}

/// Find the line index where a new `#[cfg]` + `pub use` pair should be inserted.
fn find_reexport_insertion_index(lines: &[String]) -> Option<usize> {
    let mut last_feature_pair_end = None;
    let mut last_pub_use = None;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("#[cfg(feature =") && i + 1 < lines.len() {
            let next = lines[i + 1].trim();
            if next.starts_with("pub use ") && next.ends_with(';') {
                last_feature_pair_end = Some(i + 2);
                i += 2;
                continue;
            }
        }
        if line.starts_with("pub use ") && line.ends_with(';') {
            last_pub_use = Some(i + 1);
        }
        i += 1;
    }

    last_feature_pair_end.or(last_pub_use)
}

/// Find line index where a new feature-gated macro `use` pair should be inserted.
/// Inserts after the last existing cfg+use pair inside `force_link_all_tools!`.
fn find_macro_cfg_use_insertion_index(lines: &[String]) -> Option<usize> {
    let macro_start = lines
        .iter()
        .position(|line| line.contains("macro_rules! force_link_all_tools"))?;
    let mut last_pair_end = None;

    let mut i = macro_start;
    while i + 1 < lines.len() {
        let line = lines[i].trim();
        if line == "};" {
            break;
        }
        if line.starts_with("#[cfg(feature =") {
            let next = lines[i + 1].trim();
            if next.starts_with("use $crate::") && next.ends_with(" as _;") {
                last_pair_end = Some(i + 2);
                i += 2;
                continue;
            }
        }
        i += 1;
    }

    last_pair_end
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

        let lines: Vec<String> = content.lines().map(ToString::to_string).collect();
        let pos = find_macro_cfg_use_insertion_index(&lines);
        assert!(pos.is_some());
    }
}
