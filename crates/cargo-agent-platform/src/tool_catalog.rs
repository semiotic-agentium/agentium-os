use std::{collections::BTreeMap, fs, path::Path};

use anyhow::Result;
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use regex::Regex;

use crate::workspace::find_workspace_root;

#[derive(Debug, Clone)]
pub struct CliTool {
    pub id: String,
    pub description: String,
    pub tags: Vec<String>,
    pub access: String,
}

pub fn load_cli_tools() -> Result<Vec<CliTool>> {
    let mut merged: BTreeMap<String, CliTool> = inventory_tools()
        .into_iter()
        .map(|tool| (tool.id.clone(), tool))
        .collect();

    // Workspace-defined tools should override inventory metadata so new tools appear immediately
    // even when the installed cargo subcommand binary is stale.
    for tool in workspace_tools()? {
        merged.insert(tool.id.clone(), tool);
    }

    Ok(merged.into_values().collect())
}

fn inventory_tools() -> Vec<CliTool> {
    let catalog = InventoryCatalog::new();
    let mut tools: Vec<CliTool> = catalog
        .iter()
        .map(|tool| CliTool {
            id: tool.name.to_string(),
            description: tool.description.clone(),
            tags: tool.tags.clone(),
            access: tool
                .access
                .as_ref()
                .map(|a| format!("{a:?}"))
                .unwrap_or_else(|| "None".to_string()),
        })
        .collect();
    tools.sort_by(|a, b| a.id.cmp(&b.id));
    tools
}

fn workspace_tools() -> Result<Vec<CliTool>> {
    let workspace_root = find_workspace_root()?;
    let tools_root = workspace_root.join("crates").join("tools");
    if !tools_root.exists() {
        return Ok(Vec::new());
    }

    let mut by_id: BTreeMap<String, CliTool> = BTreeMap::new();
    visit_rs_files(&tools_root, &mut |path| {
        if let Ok(content) = fs::read_to_string(path) {
            for tool in parse_baml_tool_attrs(&content) {
                by_id.insert(tool.id.clone(), tool);
            }
        }
    })?;

    Ok(by_id.into_values().collect())
}

fn visit_rs_files(root: &Path, f: &mut dyn FnMut(&Path)) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            visit_rs_files(&path, f)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            f(&path);
        }
    }
    Ok(())
}

fn parse_baml_tool_attrs(content: &str) -> Vec<CliTool> {
    let Ok(attr_re) = Regex::new(r#"(?s)#\s*\[\s*baml_tool\s*\((.*?)\)\s*\]"#) else {
        return Vec::new();
    };
    let Ok(name_re) = Regex::new(r#"name\s*=\s*"([^"]+)""#) else {
        return Vec::new();
    };
    let Ok(desc_re) = Regex::new(r#"description\s*=\s*"([^"]*)""#) else {
        return Vec::new();
    };
    let Ok(tags_re) = Regex::new(r#"(?s)tags\s*=\s*\[(.*?)\]"#) else {
        return Vec::new();
    };
    let Ok(tag_item_re) = Regex::new(r#""([^"]+)""#) else {
        return Vec::new();
    };
    let Ok(access_re) = Regex::new(r#"access\s*=\s*(Read|Write)"#) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for caps in attr_re.captures_iter(content) {
        let Some(block) = caps.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let Some(id) = name_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        else {
            continue;
        };

        let description = desc_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();

        let tags = tags_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| {
                tag_item_re
                    .captures_iter(m.as_str())
                    .filter_map(|tag_caps| tag_caps.get(1).map(|t| t.as_str().to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let access = access_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "None".to_string());

        out.push(CliTool {
            id,
            description,
            tags,
            access,
        });
    }

    out
}
