// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `build` subcommand — package an agent into a distributable tar.gz.
//!
//! This uses the builder library directly with a CLI that supports building by
//! agent name, path, or current directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, BuilderService, RuntimeTypeGenerator, StdFileSystem, StdPackager,
    TscCompiler,
};
use baml_rt_core::AgentManifest;
use console::style;

use crate::workspace::find_workspace_root;

/// Run the `build` command.
///
/// Resolves the agent directory from:
/// 1. `--path <path>` if provided (only valid when names is empty or single)
/// 2. `<names>` looks in `agents/<name>/` for each
/// 3. Current directory if neither provided
pub fn run(names: &[String], path: Option<&str>, output: Option<&str>) -> Result<()> {
    // If --path is used with multiple names, that's an error
    if path.is_some() && names.len() > 1 {
        bail!("--path cannot be used with multiple agent names");
    }

    // Collect agents to build
    let agents: Vec<PathBuf> = if names.is_empty() {
        vec![resolve_agent_dir(None, path)?]
    } else {
        names
            .iter()
            .map(|n| resolve_agent_dir(Some(n), if names.len() == 1 { path } else { None }))
            .collect::<Result<Vec<_>>>()?
    };

    let rt = tokio::runtime::Runtime::new()?;

    for agent_dir in &agents {
        let manifest = read_manifest(agent_dir)?;

        // Determine output path
        let output_path = match output {
            Some(p) => {
                let dir = PathBuf::from(p);
                let filename = format!("{}-{}.tar.gz", manifest.name, manifest.version);
                dir.join(filename)
            }
            None => {
                let filename = format!("{}-{}.tar.gz", manifest.name, manifest.version);
                std::env::current_dir()?.join(filename)
            }
        };

        println!();
        println!(
            "{} Building agent '{}'...",
            style("[1/4]").bold().dim(),
            style(&manifest.name).cyan()
        );
        println!("      Source: {}", style(agent_dir.display()).dim());
        println!("      Output: {}", style(output_path.display()).dim());

        rt.block_on(async { build_agent(agent_dir, &output_path).await })?;

        println!();
        println!("{}", style("Agent packaged successfully!").green().bold());
        println!("  Package: {}", style(output_path.display()).cyan());
    }

    if agents.len() > 1 {
        println!();
        println!(
            "{}",
            style(format!("Built {} agents successfully!", agents.len()))
                .green()
                .bold()
        );
    }

    Ok(())
}

/// Resolve the agent directory from name, path, or current directory.
fn resolve_agent_dir(name: Option<&str>, path: Option<&str>) -> Result<PathBuf> {
    // If explicit path provided, use it
    if let Some(p) = path {
        let path = PathBuf::from(p);
        if !path.exists() {
            bail!("Agent path does not exist: {}", path.display());
        }
        if !path.join("manifest.json").exists() {
            bail!(
                "Not an agent directory (missing manifest.json): {}",
                path.display()
            );
        }
        return Ok(path);
    }

    // If name provided, look in agents/ directory
    if let Some(n) = name {
        let workspace_root = find_workspace_root()?;
        let agent_path = workspace_root.join("agents").join(n);

        if !agent_path.exists() {
            // Try fixtures as fallback
            let fixture_path = workspace_root.join("tests/fixtures/agents").join(n);
            if fixture_path.exists() && fixture_path.join("manifest.json").exists() {
                return Ok(fixture_path);
            }
            bail!(
                "Agent '{}' not found in agents/ or tests/fixtures/agents/",
                n
            );
        }
        if !agent_path.join("manifest.json").exists() {
            bail!(
                "Not an agent directory (missing manifest.json): {}",
                agent_path.display()
            );
        }
        return Ok(agent_path);
    }

    // Default: current directory
    let current = std::env::current_dir()?;
    if !current.join("manifest.json").exists() {
        bail!(
            "Not an agent directory (missing manifest.json): {}\n\
             Hint: Use 'cargo agent-platform build <name>' or 'cargo agent-platform build --path <path>'",
            current.display()
        );
    }
    Ok(current)
}

/// Read and parse the agent manifest.
fn read_manifest(agent_dir: &Path) -> Result<AgentManifest> {
    let manifest_path = agent_dir.join("manifest.json");
    let content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: AgentManifest = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    Ok(manifest)
}

/// Build the agent package using the builder service.
async fn build_agent(agent_dir: &Path, output: &Path) -> Result<()> {
    let agent_dir = AgentDir::new(agent_dir.to_path_buf()).context("Failed to create AgentDir")?;
    let build_dir = BuildDir::new().context("Failed to create build directory")?;

    // Copy baml_src to build directory
    let filesystem = StdFileSystem;
    println!("{} Copying BAML sources...", style("[2/4]").bold().dim());
    baml_rt_builder::builder::FileSystem::copy_dir_all(
        &filesystem,
        &agent_dir.baml_src(),
        &build_dir.join("baml_src"),
    )?;

    // Initialize services
    let ts_compiler = TscCompiler::new();
    let type_generator = RuntimeTypeGenerator::new();
    let packager = StdPackager::new();

    let builder_service = BuilderService::new(ts_compiler, type_generator, packager);

    // Build the package
    println!(
        "{} Generating types and compiling TypeScript...",
        style("[3/4]").bold().dim()
    );
    let build_result = builder_service
        .build_package(&agent_dir, &build_dir, output)
        .await;

    if let Err(err) = build_result {
        return Err(err).context("Build failed");
    }

    println!("{} Packaging complete.", style("[4/4]").bold().dim());

    Ok(())
}
