//! `list-agents` subcommand — list all agent packages.

use std::path::PathBuf;

use anyhow::Result;
use baml_rt_core::AgentManifest;
use console::style;

use crate::{text::truncate_for_display, workspace::find_workspace_root};

/// Agent entry for display.
struct AgentEntry {
    name: String,
    version: String,
    description: String,
    #[allow(dead_code)] // Reserved for future verbose mode
    tools: Vec<String>,
    #[allow(dead_code)] // Reserved for future verbose mode
    path: PathBuf,
    source: &'static str, // "agents" or "fixtures"
}

/// Run the `list-agents` command.
pub fn run() -> Result<()> {
    let workspace_root = find_workspace_root()?;

    let roots = vec![
        ("agents", workspace_root.join("agents")),
        (
            "fixtures",
            workspace_root.join("tests").join("fixtures").join("agents"),
        ),
    ];

    let mut agents: Vec<AgentEntry> = Vec::new();

    for (label, dir) in roots {
        if !dir.exists() {
            continue;
        }

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }

            match load_agent_manifest(&manifest_path) {
                Ok(manifest) => {
                    let description = manifest
                        .discovery
                        .as_ref()
                        .and_then(|d| d.description.clone())
                        .unwrap_or_default();

                    agents.push(AgentEntry {
                        name: manifest.name,
                        version: manifest.version,
                        description,
                        tools: manifest.tools,
                        path: entry.path(),
                        source: label,
                    });
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to load {}: {}",
                        style("Warning:").yellow(),
                        manifest_path.display(),
                        e
                    );
                }
            }
        }
    }

    if agents.is_empty() {
        println!("No agents found.");
        return Ok(());
    }

    // Sort by source (agents first) then by name
    agents.sort_by(|a, b| match (a.source, b.source) {
        ("agents", "fixtures") => std::cmp::Ordering::Less,
        ("fixtures", "agents") => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });

    // Calculate column widths
    let max_name_len = agents
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(10)
        .max(10);
    let max_version_len = agents
        .iter()
        .map(|a| a.version.len())
        .max()
        .unwrap_or(7)
        .max(7);

    // Print header
    println!(
        "{:width_name$}  {:width_ver$}  {}  {}",
        style("NAME").bold().underlined(),
        style("VERSION").bold().underlined(),
        style("SOURCE").bold().underlined(),
        style("DESCRIPTION").bold().underlined(),
        width_name = max_name_len,
        width_ver = max_version_len,
    );

    // Print agents
    let mut current_source = "";
    for agent in &agents {
        // Add separator between sources
        if agent.source != current_source && !current_source.is_empty() {
            println!();
        }
        current_source = agent.source;

        let description = truncate_for_display(&agent.description, 60);

        let source_styled = match agent.source {
            "agents" => style("production").green(),
            "fixtures" => style("fixture").dim(),
            _ => style(agent.source).white(),
        };

        println!(
            "{:width_name$}  {:width_ver$}  {:10}  {}",
            style(&agent.name).cyan(),
            agent.version,
            source_styled,
            description,
            width_name = max_name_len,
            width_ver = max_version_len,
        );
    }

    println!();
    println!("Total: {} agent(s)", style(agents.len()).bold());

    // Optional: show tools for each agent with -v flag (could add later)

    Ok(())
}

/// Load an agent manifest from a file.
fn load_agent_manifest(path: &PathBuf) -> Result<AgentManifest> {
    let content = std::fs::read_to_string(path)?;
    let manifest: AgentManifest = serde_json::from_str(&content)?;
    Ok(manifest)
}
