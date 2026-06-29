// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `doctor` subcommand implementation.
//!
//! Validates workspace integrity with file checks and runner/cache catalog validation.

use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result, bail};
use baml_rt_tools::ToolCatalog;
use console::style;
use serde::Deserialize;

use crate::workspace::find_workspace_root;

/// Run the doctor command.
pub fn run(
    ci: bool,
    warn_missing_catalog: bool,
    repository_url: Option<&str>,
    snapshot_cache: Option<&str>,
) -> Result<()> {
    let workspace_root = find_workspace_root()?;
    println!(
        "{} workspace root: {}",
        style("Found").green(),
        workspace_root.display()
    );
    println!();

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Layer 1: Static checks (file-based)
    println!("{}", style("Layer 1: Static checks").bold().underlined());
    static_checks(&workspace_root, &mut errors, &mut warnings)?;

    // Layer 2: Catalog checks (runner/repository or exported snapshot cache)
    println!();
    println!("{}", style("Layer 2: Catalog checks").bold().underlined());
    catalog_checks(
        &workspace_root,
        &mut errors,
        &mut warnings,
        warn_missing_catalog,
        repository_url,
        snapshot_cache,
    )?;

    // Summary
    println!();
    if errors.is_empty() && warnings.is_empty() {
        println!("{} All checks passed!", style("✓").green().bold());
        return Ok(());
    }

    if !warnings.is_empty() {
        println!("{}", style("Warnings:").yellow().bold());
        for warning in &warnings {
            println!("  {} {}", style("⚠").yellow(), warning);
        }
    }

    if !errors.is_empty() {
        println!("{}", style("Errors:").red().bold());
        for error in &errors {
            println!("  {} {}", style("✗").red(), error);
        }

        if ci {
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Static checks that don't require compilation.
fn static_checks(
    workspace_root: &Path,
    errors: &mut Vec<String>,
    _warnings: &mut Vec<String>,
) -> Result<()> {
    // 1. Check that all tool crate directories are in workspace members
    check_workspace_members(workspace_root, errors)?;

    Ok(())
}

/// Check that all tool crate directories are in workspace members.
fn check_workspace_members(workspace_root: &Path, errors: &mut Vec<String>) -> Result<()> {
    let tools_dir = workspace_root.join("crates/tools");
    let cargo_toml_path = workspace_root.join("Cargo.toml");
    let cargo_toml =
        fs::read_to_string(&cargo_toml_path).context("Failed to read workspace Cargo.toml")?;

    if !tools_dir.exists() {
        println!("  {} No crates/tools directory found", style("⚠").yellow());
        return Ok(());
    }

    let mut found_tools = 0;
    for entry in fs::read_dir(&tools_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let tool_name = entry.file_name().to_string_lossy().to_string();
            let member_path = format!("crates/tools/{tool_name}");

            if cargo_toml.contains(&format!("\"{member_path}\""))
                || cargo_toml.contains(&format!("'{member_path}'"))
            {
                println!(
                    "  {} {} in workspace members",
                    style("✓").green(),
                    tool_name
                );
                found_tools += 1;
            } else {
                errors.push(format!(
                    "Tool '{}' not in workspace members (expected '{}')",
                    tool_name, member_path
                ));
            }
        }
    }

    println!("  Found {} tool crate(s)", found_tools);
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ToolsResponse {
    tools: Vec<ListedTool>,
}

#[derive(Debug, Deserialize)]
struct ListedTool {
    name: String,
}

/// Catalog checks use runner/repository metadata or an exported snapshot cache.
fn catalog_checks(
    workspace_root: &Path,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
    warn_missing_catalog: bool,
    repository_url: Option<&str>,
    snapshot_cache: Option<&str>,
) -> Result<()> {
    let catalog_tools = match (repository_url, snapshot_cache) {
        (Some(url), _) => Some(load_repository_tool_names(url)?),
        (None, Some(root)) => Some(load_cache_tool_names(Path::new(root))?),
        (None, None) => {
            let msg =
                "catalog tool-reference checks skipped; pass --repository-url or --snapshot-cache";
            if warn_missing_catalog {
                warnings.push(msg.to_string());
                println!("  {}", style(msg).yellow());
            } else {
                errors.push(msg.to_string());
            }
            None
        }
    };

    if let Some(catalog_tools) = &catalog_tools {
        println!("  Found {} tools in catalog", catalog_tools.len());
    }

    check_agent_manifests(
        workspace_root,
        catalog_tools.as_ref(),
        errors,
        warnings,
        warn_missing_catalog,
    )?;

    Ok(())
}

fn load_repository_tool_names(repository_url: &str) -> Result<HashSet<String>> {
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
        Ok(parsed.tools.into_iter().map(|tool| tool.name).collect())
    })
}

fn load_cache_tool_names(root: &Path) -> Result<HashSet<String>> {
    let mut tools = HashSet::new();

    let static_catalog =
        baml_rt_builder::static_tool_registry::load_static_tool_catalog_from_cache(root)
            .with_context(|| {
                format!(
                    "loading static tool catalog from {}",
                    baml_rt_builder::static_tool_registry::static_tool_catalog_path(root).display()
                )
            })?;
    tools.extend(static_catalog.iter().map(|tool| tool.name.to_string()));

    let external_root = baml_rt_tools::external_tool_cache::resolve_cache_root(root);
    for snapshot in baml_rt_tools::external_tool_cache::read_approved_snapshots(&external_root)? {
        tools.insert(snapshot.tool.name);
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
            tools.extend(
                snapshot
                    .tools
                    .into_iter()
                    .map(|tool| tool.platform_tool_name),
            );
        }
    }

    Ok(tools)
}

/// Check that agent manifests reference tools that exist in the catalog.
fn check_agent_manifests(
    workspace_root: &Path,
    catalog_tools: Option<&HashSet<String>>,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
    warn_missing_catalog: bool,
) -> Result<()> {
    let agents_dirs = [
        workspace_root.join("agents"),
        workspace_root.join("tests/fixtures/agents"),
    ];

    for agents_dir in &agents_dirs {
        if !agents_dir.exists() {
            continue;
        }

        for entry in fs::read_dir(agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }

            let agent_name = entry.file_name().to_string_lossy().to_string();
            let manifest_content = fs::read_to_string(&manifest_path)?;

            // Parse manifest JSON to extract tags/tools checks
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest_content) {
                validate_manifest_tags(&agent_name, &manifest, errors, warnings);

                if let Some(tools) = manifest.get("tools").and_then(|t| t.as_array()) {
                    for tool in tools {
                        if let Some(tool_name) = tool.as_str()
                            && let Some(catalog_tools) = catalog_tools
                            && !catalog_tools.contains(tool_name)
                        {
                            let msg = format!(
                                "{}: tool '{}' not found in catalog",
                                agent_name, tool_name
                            );
                            if warn_missing_catalog {
                                warnings.push(msg);
                            } else {
                                errors.push(msg);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn validate_manifest_tags(
    agent_name: &str,
    manifest: &serde_json::Value,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let Some(tags_val) = manifest.get("tags") else {
        errors.push(format!("{agent_name}: missing required 'tags' field"));
        return;
    };

    let Some(tags) = tags_val.as_array() else {
        errors.push(format!("{agent_name}: 'tags' must be an array"));
        return;
    };

    if tags.is_empty() {
        errors.push(format!("{agent_name}: 'tags' must be a non-empty array"));
        return;
    }

    let mut seen: HashSet<String> = HashSet::new();
    for (idx, tag_val) in tags.iter().enumerate() {
        let Some(raw_tag) = tag_val.as_str() else {
            errors.push(format!(
                "{agent_name}: tags[{idx}] must be a string, got {}",
                value_type_name(tag_val)
            ));
            continue;
        };

        let tag = raw_tag.trim();
        if tag.is_empty() {
            errors.push(format!("{agent_name}: tags[{idx}] cannot be empty"));
            continue;
        }

        if tag.chars().any(char::is_whitespace) {
            errors.push(format!(
                "{agent_name}: tags[{idx}] contains spaces/whitespace ('{raw_tag}')"
            ));
            continue;
        }

        if !is_valid_tag_shape(tag) {
            errors.push(format!(
                "{agent_name}: tags[{idx}] has invalid format ('{raw_tag}'); expected one-word parts separated by '-' or '_'"
            ));
            continue;
        }

        let tag_lc = tag.to_ascii_lowercase();
        if !seen.insert(tag_lc.clone()) {
            errors.push(format!(
                "{agent_name}: duplicate tag '{raw_tag}' (case-insensitive)"
            ));
        }

        if tag != tag_lc {
            warnings.push(format!(
                "{agent_name}: non-normalized tag '{raw_tag}' (prefer lowercase)"
            ));
        }
    }
}

fn is_valid_tag_shape(tag: &str) -> bool {
    // Allowed: one-word parts [a-zA-Z0-9]+ separated by '-' or '_'.
    // Disallow leading/trailing separators and consecutive separators.
    let mut prev_sep = false;
    let mut saw_alnum = false;

    for (idx, ch) in tag.chars().enumerate() {
        let is_sep = ch == '-' || ch == '_';
        if ch.is_ascii_alphanumeric() {
            saw_alnum = true;
            prev_sep = false;
            continue;
        }
        if is_sep {
            if idx == 0 || prev_sep {
                return false;
            }
            prev_sep = true;
            continue;
        }
        return false;
    }

    saw_alnum && !prev_sep
}

fn value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}
