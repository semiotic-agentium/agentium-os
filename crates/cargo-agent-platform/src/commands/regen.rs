//! `regen` subcommand — regenerate generated_tools.baml and baml-runtime.d.ts for all agents.

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use console::style;

/// Run the `regen` command.
pub fn run() -> Result<()> {
    let workspace_root = find_workspace_root()?;

    let roots = vec![
        ("agents", workspace_root.join("agents")),
        (
            "fixtures",
            workspace_root.join("tests").join("fixtures").join("agents"),
        ),
    ];

    let mut total_count = 0;

    for (label, dir) in roots {
        if !dir.exists() {
            println!(
                "{} Skipping {} (directory does not exist)",
                style("Note:").yellow(),
                style(dir.display()).dim()
            );
            continue;
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("baml_src").is_dir())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        if entries.is_empty() {
            println!(
                "{} No agents found in {}",
                style("Note:").yellow(),
                style(dir.display()).dim()
            );
            continue;
        }

        println!(
            "{} Regenerating {} agents in {}...",
            style("[regen]").bold().dim(),
            entries.len(),
            style(label).cyan()
        );

        for entry in &entries {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            print!("  {} {}... ", style("->").dim(), name_str);
            std::io::stdout().flush().ok();

            match regen_agent(&entry.path()) {
                Ok(()) => {
                    println!("{}", style("ok").green());
                    total_count += 1;
                }
                Err(e) => {
                    println!("{}", style("failed").red());
                    eprintln!("     Error: {}", e);
                }
            }
        }
    }

    println!();
    println!(
        "{} Regenerated {} agent(s)",
        style("Done!").green().bold(),
        total_count
    );

    Ok(())
}

/// Find the workspace root by looking for Cargo.toml with [workspace].
fn find_workspace_root() -> Result<PathBuf> {
    let mut current = std::env::current_dir()?;
    loop {
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = std::fs::read_to_string(&cargo_toml)?;
            if content.contains("[workspace]") {
                return Ok(current);
            }
        }
        if !current.pop() {
            bail!("Could not find workspace root (Cargo.toml with [workspace] section)");
        }
    }
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
            .map_err(|e| anyhow::anyhow!("Type generation failed: {}", e))?;

        // Sync generated_*.baml tool interfaces back into the agent's baml_src
        sync_generated_baml_files(&build_dir, &agent_dir.baml_src())
    })
}

/// Sync generated_*.baml files from build_dir to agent's baml_src.
fn sync_generated_baml_files(
    build_dir: &baml_rt_builder::builder::BuildDir,
    dest_baml_src: &Path,
) -> Result<()> {
    let generated_src_dir = build_dir.join("baml_src");
    if !generated_src_dir.is_dir() {
        // No generated files to sync - this is not an error
        return Ok(());
    }

    std::fs::create_dir_all(dest_baml_src)?;

    let mut generated_names: HashSet<String> = HashSet::new();
    for entry in std::fs::read_dir(&generated_src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !file_name.starts_with("generated_") || !file_name.ends_with(".baml") {
            continue;
        }

        generated_names.insert(file_name.to_string());
        let data = std::fs::read(&path)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dest_baml_src)?;
        tmp.write_all(&data)?;
        let dest_path = dest_baml_src.join(file_name);
        tmp.persist(&dest_path).map_err(|e| e.error)?;
    }

    // Remove stale generated_*.baml files that are no longer emitted by the builder
    for entry in std::fs::read_dir(dest_baml_src)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.starts_with("generated_")
            && file_name.ends_with(".baml")
            && !generated_names.contains(file_name)
        {
            std::fs::remove_file(path)?;
        }
    }

    Ok(())
}
