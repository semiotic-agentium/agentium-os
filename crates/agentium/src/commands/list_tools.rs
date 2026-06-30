// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `list-tools` subcommand implementation.
//!
//! Lists tools from the runner/repository catalog by default, or from the unified
//! offline snapshot cache when `--snapshot-cache` is supplied.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use baml_rt_tools::ToolCatalog;
use console::style;
use serde::Deserialize;

use crate::text::truncate_for_display;

#[derive(Debug, Clone, Deserialize)]
pub struct ListedTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub access: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct ToolsResponse {
    tools: Vec<ListedTool>,
}

pub fn run(repository_url: &str, snapshot_cache: Option<&str>) -> Result<()> {
    let tools = match snapshot_cache {
        Some(root) => load_cache_tools(Path::new(root))?,
        None => fetch_repository_tools(repository_url)?,
    };
    print_tools(tools)
}

fn fetch_repository_tools(repository_url: &str) -> Result<Vec<ListedTool>> {
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
        Ok(parsed.tools)
    })
}

fn load_cache_tools(root: &Path) -> Result<Vec<ListedTool>> {
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
        tools.push(ListedTool {
            name: tool.name.to_string(),
            description: tool.description.clone(),
            tags: tool.tags.clone(),
            access: tool
                .access
                .as_ref()
                .map(|a| format!("{a:?}"))
                .unwrap_or_else(|| "None".to_string()),
            source: "static".to_string(),
        });
    }

    let external_root = baml_rt_tools::external_tool_cache::resolve_cache_root(root);
    for snapshot in baml_rt_tools::external_tool_cache::read_approved_snapshots(&external_root)? {
        tools.push(ListedTool {
            name: snapshot.tool.name,
            description: snapshot.tool.description,
            tags: snapshot.tool.tags,
            access: format!("{:?}", snapshot.tool.access_level),
            source: "external".to_string(),
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
                tools.push(ListedTool {
                    name: tool.platform_tool_name,
                    description: tool.description.unwrap_or_default(),
                    tags: vec!["mcp".to_string(), snapshot.server_id.clone()],
                    access: format!("{:?}", tool.access_level),
                    source: "mcp".to_string(),
                });
            }
        }
    }

    tools.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.source.cmp(&b.source)));
    Ok(tools)
}

fn print_tools(tools: Vec<ListedTool>) -> Result<()> {
    let total = tools.len();

    if tools.is_empty() {
        println!("{}", style("No tools found.").yellow());
        return Ok(());
    }

    println!(
        "{:<30} {:<10} {:<50} {:<25} {}",
        style("NAME").bold().underlined(),
        style("SOURCE").bold().underlined(),
        style("DESCRIPTION").bold().underlined(),
        style("TAGS").bold().underlined(),
        style("ACCESS").bold().underlined()
    );

    for tool in tools {
        let description = truncate_for_display(&tool.description, 48);
        let tags = format!("[{}]", tool.tags.join(", "));

        println!(
            "{:<30} {:<10} {:<50} {:<25} {}",
            tool.name, tool.source, description, tags, tool.access
        );
    }

    println!();
    println!("{} {} tool(s) registered", style("Total:").bold(), total);

    Ok(())
}
