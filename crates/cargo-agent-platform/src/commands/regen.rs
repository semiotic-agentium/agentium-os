//! `regen` subcommand — regenerate runtime BAML prelude and baml-runtime.d.ts for agents.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};
use console::style;

use crate::{generated_baml::sync_generated_baml_files, workspace::find_workspace_root};

/// Run the `regen` command.
///
/// If `names` is empty, regenerates all agents.
/// If `names` is provided, only regenerates the specified agents.
pub fn run(names: &[String]) -> Result<()> {
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

            match regen_agent(&entry.path()) {
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

/// Regenerate types for a single agent.
fn regen_agent(root: &Path) -> Result<()> {
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
        let generator = RuntimeTypeGenerator::new();
        // generate() writes src/baml-runtime.d.ts directly into the agent's source tree
        generator
            .generate(&agent_dir, &build_dir)
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("Tool metadata missing for:") {
                    let external_dir = std::env::var("BAML_EXTERNAL_TOOLS_DIR")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or_else(|| "(not set)".to_string());
                    anyhow::anyhow!(
                        "Type generation failed: {msg}\nHint: if the agent uses external tools, set BAML_EXTERNAL_TOOLS_DIR to the external tool directory (current: {external_dir}).\nHint: run the workspace binary to avoid stale installed subcommands: cargo run -p cargo-agent-platform -- regen <agent>."
                    )
                } else {
                    anyhow::anyhow!("Type generation failed: {msg}")
                }
            })?;

        // Sync generated BAML artifacts (including _baml_runtime.baml) back into agent baml_src
        sync_generated_baml_files(&build_dir, &agent_dir.baml_src())
    })
}
