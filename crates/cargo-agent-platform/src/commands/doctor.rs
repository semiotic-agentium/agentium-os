//! `doctor` subcommand implementation.
//!
//! Validates workspace integrity with static checks and catalog validation.

use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result};
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use console::style;

use crate::workspace::find_workspace_root;

/// Run the doctor command.
pub fn run(ci: bool, warn_missing_catalog: bool) -> Result<()> {
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

    // Layer 2: Catalog checks (requires compiled inventory)
    println!();
    println!("{}", style("Layer 2: Catalog checks").bold().underlined());
    catalog_checks(
        &workspace_root,
        &mut errors,
        &mut warnings,
        warn_missing_catalog,
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

    // 2. Check that all tool crates have matching entries in baml-tool-links/Cargo.toml
    check_tool_links_deps(workspace_root, errors)?;

    // 3. Check that all tool crates have entries in force_link_all_tools! macro
    check_force_link_macro(workspace_root, errors)?;

    // 4. Check feature forwarding in runner/builder
    check_feature_forwarding(workspace_root, errors)?;

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

/// Check that all tool crates have matching deps in baml-tool-links.
fn check_tool_links_deps(workspace_root: &Path, errors: &mut Vec<String>) -> Result<()> {
    let tools_dir = workspace_root.join("crates/tools");
    let tool_links_toml = workspace_root.join("crates/baml-tool-links/Cargo.toml");

    if !tool_links_toml.exists() {
        errors.push("baml-tool-links/Cargo.toml not found".to_string());
        return Ok(());
    }

    let content = fs::read_to_string(&tool_links_toml)?;

    // Core tools that should always be unconditional dependencies
    // (reserved for future use in enhanced checking)
    let _core_tools = ["calculator", "claude", "system"];

    for entry in fs::read_dir(&tools_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let tool_name = entry.file_name().to_string_lossy().to_string();

            // Skip internal-dev (test-only)
            if tool_name == "internal-dev" {
                continue;
            }

            let dep_name = format!("baml-tools-{tool_name}");

            // Handle the special case where claude is "baml-rt-tools-claude"
            let actual_dep_name = if tool_name == "claude" {
                "baml-rt-tools-claude".to_string()
            } else {
                dep_name.clone()
            };

            if content.contains(&actual_dep_name) {
                println!(
                    "  {} {} in baml-tool-links deps",
                    style("✓").green(),
                    tool_name
                );
            } else {
                errors.push(format!(
                    "Tool '{}' missing from baml-tool-links/Cargo.toml dependencies",
                    tool_name
                ));
            }
        }
    }

    Ok(())
}

/// Check that all tool crates have entries in force_link_all_tools! macro.
fn check_force_link_macro(workspace_root: &Path, errors: &mut Vec<String>) -> Result<()> {
    let tools_dir = workspace_root.join("crates/tools");
    let lib_rs_path = workspace_root.join("crates/baml-tool-links/src/lib.rs");

    if !lib_rs_path.exists() {
        errors.push("baml-tool-links/src/lib.rs not found".to_string());
        return Ok(());
    }

    let content = fs::read_to_string(&lib_rs_path)?;

    for entry in fs::read_dir(&tools_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let tool_name = entry.file_name().to_string_lossy().to_string();
            let crate_name = format!("baml_tools_{}", tool_name.replace('-', "_"));

            // Handle special cases
            let actual_crate_name = if tool_name == "claude" {
                "baml_rt_tools_claude".to_string()
            } else if tool_name == "internal-dev" {
                "baml_tools_internal_dev".to_string()
            } else {
                crate_name.clone()
            };

            if content.contains(&actual_crate_name) {
                println!(
                    "  {} {} in force_link_all_tools! macro",
                    style("✓").green(),
                    tool_name
                );
            } else {
                errors.push(format!(
                    "Tool '{}' missing from force_link_all_tools! macro (expected '{}')",
                    tool_name, actual_crate_name
                ));
            }
        }
    }

    Ok(())
}

/// Check that runner and builder have feature forwarding for all tools.
fn check_feature_forwarding(workspace_root: &Path, _errors: &mut Vec<String>) -> Result<()> {
    let tool_links_toml = workspace_root.join("crates/baml-tool-links/Cargo.toml");
    let runner_toml = workspace_root.join("crates/baml-agent-runner/Cargo.toml");
    let builder_toml = workspace_root.join("crates/baml-rt-builder/Cargo.toml");

    let tool_links_content = fs::read_to_string(&tool_links_toml)?;
    let runner_content = fs::read_to_string(&runner_toml)?;
    let builder_content = fs::read_to_string(&builder_toml)?;

    // Extract feature names from baml-tool-links (excluding default, http-tools, all-tools)
    let excluded_features = ["default", "http-tools", "all-tools"];
    let tool_features: Vec<String> = extract_features(&tool_links_content)
        .into_iter()
        .filter(|f| !excluded_features.contains(&f.as_str()))
        .collect();

    for feature in &tool_features {
        let forward_pattern = format!("{feature} = [\"baml-tool-links/{feature}\"]");
        let forward_pattern_alt = format!("{feature} = ['baml-tool-links/{feature}']");

        // Check runner
        if runner_content.contains(&forward_pattern)
            || runner_content.contains(&forward_pattern_alt)
        {
            println!(
                "  {} {} feature forwarding in runner",
                style("✓").green(),
                feature
            );
        } else if !runner_content.contains(&format!("{feature} = ")) {
            // Feature not present at all - may be intentional for some tools
            println!(
                "  {} {} not in runner (may be intentional)",
                style("⚠").yellow(),
                feature
            );
        }

        // Check builder
        if builder_content.contains(&forward_pattern)
            || builder_content.contains(&forward_pattern_alt)
        {
            println!(
                "  {} {} feature forwarding in builder",
                style("✓").green(),
                feature
            );
        } else if !builder_content.contains(&format!("{feature} = ")) {
            println!(
                "  {} {} not in builder (may be intentional)",
                style("⚠").yellow(),
                feature
            );
        }
    }

    Ok(())
}

/// Extract feature names from Cargo.toml content.
fn extract_features(content: &str) -> Vec<String> {
    let mut features = Vec::new();
    let mut in_features = false;

    for line in content.lines() {
        if line.trim() == "[features]" {
            in_features = true;
            continue;
        }
        if in_features {
            if line.starts_with('[') {
                break;
            }
            if let Some(name) = line.split('=').next() {
                let name = name.trim();
                if !name.is_empty() {
                    features.push(name.to_string());
                }
            }
        }
    }

    features
}

/// Catalog checks that require the compiled inventory.
fn catalog_checks(
    workspace_root: &Path,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
    warn_missing_catalog: bool,
) -> Result<()> {
    let catalog = InventoryCatalog::new();
    let tool_count = catalog.iter().count();

    println!("  Found {} tools in inventory", tool_count);

    // Collect all tool names from inventory
    let inventory_tools: HashSet<String> = catalog.iter().map(|t| t.name.to_string()).collect();

    // Check agent manifests reference valid tools
    check_agent_manifests(
        workspace_root,
        &inventory_tools,
        errors,
        warnings,
        warn_missing_catalog,
    )?;

    Ok(())
}

/// Check that agent manifests reference tools that exist in the catalog.
fn check_agent_manifests(
    workspace_root: &Path,
    inventory_tools: &HashSet<String>,
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

            // Parse manifest JSON to extract tools
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest_content)
                && let Some(tools) = manifest.get("tools").and_then(|t| t.as_array())
            {
                for tool in tools {
                    if let Some(tool_name) = tool.as_str()
                        && !inventory_tools.contains(tool_name)
                    {
                        let msg =
                            format!("{}: tool '{}' not found in catalog", agent_name, tool_name);
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

    Ok(())
}
