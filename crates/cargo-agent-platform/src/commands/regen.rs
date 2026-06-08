// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `regen` subcommand — regenerate runtime BAML prelude and baml-runtime.d.ts for agents.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use console::style;

use crate::{generated_baml::sync_generated_baml_files, workspace::find_workspace_root};

/// Run the `regen` command.
///
/// If `paths` is provided, regenerates exactly those agent directories.
/// Otherwise, if `names` is provided, regenerates matching agents under workspace roots.
/// With neither `paths` nor `names`, regenerates all discovered agents.
pub fn run(
    names: &[String],
    paths: &[String],
    repository_url: &str,
    snapshot_cache: Option<&str>,
) -> Result<()> {
    if !paths.is_empty() && !names.is_empty() {
        bail!("--path cannot be combined with agent names");
    }

    if let Some(cache) = snapshot_cache {
        println!(
            "{} Resolving snapshots from offline cache {} (registry not contacted).",
            style("Note:").yellow(),
            style(cache).dim()
        );
    }

    if !paths.is_empty() {
        return run_explicit_paths(paths, repository_url, snapshot_cache);
    }

    let workspace_root = find_workspace_root()?;

    let filter: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let filter_active = !filter.is_empty();

    let roots = vec![
        ("agents", workspace_root.join("agents")),
        (
            "fixtures",
            workspace_root.join("tests").join("fixtures").join("agents"),
        ),
    ];

    let mut total_count = 0;
    let mut failed_count = 0;
    let mut not_found: HashSet<&str> = filter.clone();

    for (label, dir) in roots {
        if !dir.exists() {
            if !filter_active {
                println!(
                    "{} Skipping {} (directory does not exist)",
                    style("Note:").yellow(),
                    style(dir.display()).dim()
                );
            }
            continue;
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("baml_src").is_dir())
            .filter(|e| {
                if filter_active {
                    let name = e.file_name();
                    let name_str = name.to_string_lossy();
                    filter.contains(name_str.as_ref())
                } else {
                    true
                }
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        if entries.is_empty() {
            if !filter_active {
                println!(
                    "{} No agents found in {}",
                    style("Note:").yellow(),
                    style(dir.display()).dim()
                );
            }
            continue;
        }

        println!(
            "{} Regenerating {} agent(s) in {}...",
            style("[regen]").bold().dim(),
            entries.len(),
            style(label).cyan()
        );

        for entry in &entries {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Track that we found this agent
            not_found.remove(name_str.as_ref());

            print!("  {} {}... ", style("->").dim(), name_str);
            let mut stdout = std::io::stdout();
            let _ = std::io::Write::flush(&mut stdout);

            match regen_agent(&entry.path(), repository_url, snapshot_cache) {
                Ok(()) => {
                    println!("{}", style("ok").green());
                    total_count += 1;
                }
                Err(e) => {
                    println!("{}", style("failed").red());
                    eprintln!("     Error: {}", e);
                    failed_count += 1;
                }
            }
        }
    }

    // Warn about agents that weren't found
    if !not_found.is_empty() {
        println!();
        println!("{} Agent(s) not found:", style("Warning:").yellow().bold());
        for name in &not_found {
            println!("  - {}", style(name).red());
        }
        println!();
        println!(
            "Available agents can be listed with: {}",
            style("cargo agent-platform list-agents").cyan()
        );
    }

    println!();
    println!(
        "{} Regenerated {} agent(s)",
        style("Done!").green().bold(),
        total_count
    );

    if failed_count > 0 {
        bail!("Regen failed for {failed_count} agent(s). Fix reported errors and retry.");
    }

    Ok(())
}

fn run_explicit_paths(
    paths: &[String],
    repository_url: &str,
    snapshot_cache: Option<&str>,
) -> Result<()> {
    let mut unique_paths = HashSet::new();
    for raw in paths {
        let canonical = PathBuf::from(raw)
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize path: {}", raw))?;
        validate_agent_dir(&canonical)?;
        unique_paths.insert(canonical);
    }

    let mut sorted_paths: Vec<PathBuf> = unique_paths.into_iter().collect();
    sorted_paths.sort();

    let mut failed_count = 0;
    let mut total_count = 0;

    println!(
        "{} Regenerating {} explicit path(s)...",
        style("[regen]").bold().dim(),
        sorted_paths.len()
    );

    for path in &sorted_paths {
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        print!(
            "  {} {} ({})... ",
            style("->").dim(),
            display_name,
            style(path.display()).dim()
        );
        let mut stdout = std::io::stdout();
        let _ = std::io::Write::flush(&mut stdout);

        match regen_agent(path, repository_url, snapshot_cache) {
            Ok(()) => {
                println!("{}", style("ok").green());
                total_count += 1;
            }
            Err(e) => {
                println!("{}", style("failed").red());
                eprintln!("     Error: {}", e);
                failed_count += 1;
            }
        }
    }

    println!();
    println!(
        "{} Regenerated {} agent(s)",
        style("Done!").green().bold(),
        total_count
    );

    if failed_count > 0 {
        bail!("Regen failed for {failed_count} agent(s). Fix reported errors and retry.");
    }

    Ok(())
}

fn validate_agent_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("Agent path does not exist: {}", path.display());
    }
    if !path.is_dir() {
        bail!("Agent path is not a directory: {}", path.display());
    }
    if !path.join("baml_src").is_dir() {
        bail!(
            "Not an agent directory (missing baml_src/): {}",
            path.display()
        );
    }
    Ok(())
}

/// Regenerate types for a single agent.
fn regen_agent(root: &Path, repository_url: &str, snapshot_cache: Option<&str>) -> Result<()> {
    use baml_rt_builder::builder::{
        AgentDir, BuildDir, RuntimeTypeGenerator, compiler::write_canonical_tsconfig,
        traits::TypeGenerator,
    };

    // Ensure canonical tsconfig.json
    write_canonical_tsconfig(root).context("Failed to write tsconfig.json")?;

    let agent_dir = AgentDir::new(root.to_path_buf()).context("Failed to create AgentDir")?;
    let build_dir = BuildDir::new().context("Failed to create BuildDir")?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let generator = match snapshot_cache {
            Some(root) => RuntimeTypeGenerator::with_snapshot_cache(root),
            None => RuntimeTypeGenerator::with_registry_url(repository_url),
        };
        // generate() writes src/baml-runtime.d.ts directly into the agent's source tree
        generator
            .generate(&agent_dir, &build_dir)
            .await
            .map_err(|e| {
                let msg = {
                    let mut s = e.to_string();
                    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
                    while let Some(cause) = src {
                        s.push_str("\n  caused by: ");
                        s.push_str(&cause.to_string());
                        src = cause.source();
                    }
                    s
                };
                if msg.contains("Tool metadata missing for:") {
                    anyhow::anyhow!(
                        "Type generation failed: {msg}\nHint: import/approve required MCP or external-tool snapshots in repository registry, then run with --repository-url {repository_url}.\nHint: run the workspace binary to avoid stale installed subcommands: cargo run -p cargo-agent-platform -- regen <agent>."
                    )
                } else {
                    anyhow::anyhow!("Type generation failed: {msg}")
                }
            })?;

        // Sync generated BAML artifacts (including _baml_runtime.baml) back into agent baml_src
        sync_generated_baml_files(&build_dir, &agent_dir.baml_src())
    })
}
