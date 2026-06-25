// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use baml_rt_tools::ToolCatalog;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct CliTool {
    pub id: String,
    pub description: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum ToolPickerSource {
    Repository { url: String },
    SnapshotCache { root: String },
}

pub fn load_tools_for_picker(source: &ToolPickerSource) -> Result<Vec<CliTool>> {
    Ok(canonicalize_tools(load_tools(source)?))
}

pub fn load_tools(source: &ToolPickerSource) -> Result<Vec<CliTool>> {
    match source {
        ToolPickerSource::Repository { url } => load_repository_tools(url),
        ToolPickerSource::SnapshotCache { root } => load_cache_tools(Path::new(root)),
    }
}

#[derive(Debug, Deserialize)]
struct ToolsResponse {
    tools: Vec<RepositoryTool>,
}

#[derive(Debug, Deserialize)]
struct RepositoryTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
}

fn load_repository_tools(repository_url: &str) -> Result<Vec<CliTool>> {
    let url = format!("{}/tools", repository_url.trim_end_matches('/'));
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let response = reqwest::Client::new()
            .get(url.as_str())
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GET {url} failed ({status}): {body}");
        }
        let parsed: ToolsResponse =
            serde_json::from_str(&body).with_context(|| format!("parsing response from {url}"))?;
        let mut tools: Vec<CliTool> = parsed
            .tools
            .into_iter()
            .map(|tool| CliTool {
                id: tool.name,
                description: tool.description,
                tags: tool.tags,
            })
            .collect();
        tools.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(tools)
    })
}

fn load_cache_tools(root: &Path) -> Result<Vec<CliTool>> {
    let mut tools = Vec::new();

    let static_catalog =
        baml_rt_builder::static_tool_registry::load_static_tool_catalog_from_cache(root)
            .with_context(|| {
                format!(
                    "loading static tool catalog from {}",
                    baml_rt_builder::static_tool_registry::static_tool_catalog_path(root).display()
                )
            })?;
    for tool in static_catalog.iter() {
        tools.push(CliTool {
            id: tool.name.to_string(),
            description: tool.description.clone(),
            tags: tool.tags.clone(),
        });
    }

    let external_root = baml_rt_tools::external_tool_cache::resolve_cache_root(root);
    for snapshot in baml_rt_tools::external_tool_cache::read_approved_snapshots(&external_root)? {
        tools.push(CliTool {
            id: snapshot.tool.name,
            description: snapshot.tool.description,
            tags: snapshot.tool.tags,
        });
    }

    let mcp_root = baml_rt_tools::mcp_cache::resolve_cache_root(root);
    let servers_dir = mcp_root.join("servers");
    if servers_dir.exists() {
        for entry in fs::read_dir(&servers_dir)
            .with_context(|| format!("reading MCP cache servers from {}", servers_dir.display()))?
        {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let server_id = entry.file_name().to_string_lossy().to_string();
            let snapshot = baml_rt_tools::mcp_cache::read_snapshot(&mcp_root, &server_id)
                .with_context(|| format!("reading MCP cache snapshot {server_id}"))?;
            if !snapshot.approval.state.is_approved() {
                continue;
            }
            for tool in snapshot.tools {
                tools.push(CliTool {
                    id: tool.platform_tool_name,
                    description: tool.description.unwrap_or_default(),
                    tags: vec!["mcp".to_string(), snapshot.server_id.clone()],
                });
            }
        }
    }

    tools.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(tools)
}

fn canonicalize_tools(tools: Vec<CliTool>) -> Vec<CliTool> {
    let mut by_family: BTreeMap<(String, String), Vec<CliTool>> = BTreeMap::new();
    let mut passthrough = Vec::new();

    for tool in tools {
        if let Some((bundle, local)) = split_tool_id(&tool.id) {
            let root = family_root(local);
            by_family
                .entry((bundle.to_string(), root.to_string()))
                .or_default()
                .push(tool);
        } else {
            passthrough.push(tool);
        }
    }

    let mut out = passthrough;
    for ((_bundle, root), mut group) in by_family {
        if group.len() == 1 {
            out.push(group.remove(0));
            continue;
        }

        group.sort_by(|a, b| {
            let la = split_tool_id(&a.id)
                .map(|(_, local)| local)
                .unwrap_or_default();
            let lb = split_tool_id(&b.id)
                .map(|(_, local)| local)
                .unwrap_or_default();
            let a_is_root = la == root;
            let b_is_root = lb == root;

            b_is_root
                .cmp(&a_is_root)
                .then_with(|| la.len().cmp(&lb.len()))
                .then_with(|| a.id.cmp(&b.id))
        });
        out.push(group.remove(0));
    }

    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn canonicalize_tool_ids(ids: &[String]) -> Vec<String> {
    let mut by_family: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut passthrough = Vec::new();

    for id in ids {
        if let Some((bundle, local)) = split_tool_id(id) {
            let root = family_root(local);
            by_family
                .entry((bundle.to_string(), root.to_string()))
                .or_default()
                .push(id.clone());
        } else {
            passthrough.push(id.clone());
        }
    }

    let mut out = passthrough;
    for ((_bundle, root), mut group) in by_family {
        if group.len() == 1 {
            out.push(group.remove(0));
            continue;
        }

        group.sort_by(|a, b| {
            let la = split_tool_id(a).map(|(_, local)| local).unwrap_or_default();
            let lb = split_tool_id(b).map(|(_, local)| local).unwrap_or_default();
            let a_is_root = la == root;
            let b_is_root = lb == root;
            b_is_root
                .cmp(&a_is_root)
                .then_with(|| la.len().cmp(&lb.len()))
                .then_with(|| a.cmp(b))
        });
        out.push(group.remove(0));
    }

    out.sort();
    out.dedup();
    out
}

fn split_tool_id(id: &str) -> Option<(&str, &str)> {
    let (bundle, local) = id.split_once('/')?;
    if bundle.is_empty() || local.is_empty() {
        return None;
    }
    Some((bundle, local))
}

fn family_root(local: &str) -> &str {
    for (idx, ch) in local.char_indices() {
        if ch.is_ascii_uppercase() {
            return &local[..idx];
        }
    }
    local
}
