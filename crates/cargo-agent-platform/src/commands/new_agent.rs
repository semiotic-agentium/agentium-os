//! `new-agent` subcommand — create a new agent package.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use console::style;

use crate::templates::{agent_coordinator, agent_planner, agent_simple};

/// Agent template type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTemplate {
    /// Simple agent without tools (Q&A, chatbot).
    Simple,
    /// Basic agent with tool support.
    BasicTools,
    /// 3-phase planner: Intent -> Plan -> Execute.
    Planner,
    /// Multi-agent coordinator/delegator.
    Coordinator,
}

impl AgentTemplate {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "simple" => Some(Self::Simple),
            "basic-tools" | "basic_tools" | "basictools" => Some(Self::BasicTools),
            "planner" => Some(Self::Planner),
            "coordinator" => Some(Self::Coordinator),
            _ => None,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::BasicTools => "basic-tools",
            Self::Planner => "planner",
            Self::Coordinator => "coordinator",
        }
    }
}

/// Run the `new-agent` command.
pub fn run(
    name: &str,
    tools: Option<&str>,
    template: &str,
    description: &str,
    output: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    // Validate agent name
    let slug = validate_agent_name(name)?;

    // Parse tools
    let tool_ids: Vec<String> = match tools {
        Some(t) if !t.trim().is_empty() => t
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };

    // Determine template
    let template = determine_template(template, &tool_ids)?;

    // Find workspace root and determine output directory
    let workspace_root = find_workspace_root()?;
    let output_dir = match output {
        Some(p) => PathBuf::from(p),
        None => workspace_root.join("agents").join(&slug),
    };

    if dry_run {
        print_dry_run_summary(&slug, &tool_ids, template, description, &output_dir);
        return Ok(());
    }

    // Check if directory already exists
    if output_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&output_dir)?.collect();
        if !entries.is_empty() {
            bail!(
                "Directory already exists and is non-empty: {}",
                output_dir.display()
            );
        }
    }

    // Create agent based on template
    println!(
        "{} Creating agent '{}' with template '{}'...",
        style("[1/2]").bold().dim(),
        style(&slug).cyan(),
        style(template.name()).green()
    );

    match template {
        AgentTemplate::Simple | AgentTemplate::BasicTools => {
            create_basic_agent(&output_dir, &slug, description, &tool_ids)?;
        }
        AgentTemplate::Planner => {
            create_planner_agent(&output_dir, &slug, description, &tool_ids)?;
        }
        AgentTemplate::Coordinator => {
            create_coordinator_agent(&output_dir, &slug, description)?;
        }
    }

    println!("{} Running type generation...", style("[2/2]").bold().dim());

    // Run type generation
    run_type_generation(&output_dir)?;

    println!();
    println!("{}", style("Agent created successfully!").green().bold());
    println!();
    println!("  Location: {}", style(output_dir.display()).cyan());
    println!();
    println!("{}", style("Next steps:").bold());
    println!(
        "  1. Edit {} to customize your agent logic",
        style("src/index.ts").cyan()
    );
    println!(
        "  2. Edit {} to customize your BAML prompts",
        style("baml_src/").cyan()
    );
    println!(
        "  3. Run {} to package the agent",
        style("cargo run -p baml-rt-builder --bin baml-agent-builder").dim()
    );

    Ok(())
}

/// Validate agent name and return the slug.
fn validate_agent_name(name: &str) -> Result<String> {
    // Convert to slug (kebab-case)
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        bail!("Agent name must contain at least one alphanumeric character");
    }

    // Check for reserved names
    let reserved = ["test", "lib", "bin", "build", "dev", "src", "agents"];
    if reserved.contains(&slug.as_str()) {
        bail!(
            "'{}' is a reserved name and cannot be used as an agent name",
            slug
        );
    }

    Ok(slug)
}

/// Determine the template to use.
fn determine_template(template_str: &str, tool_ids: &[String]) -> Result<AgentTemplate> {
    if let Some(t) = AgentTemplate::from_str(template_str) {
        // Validate template/tools combination
        match t {
            AgentTemplate::Simple if !tool_ids.is_empty() => {
                println!(
                    "{} Template 'simple' specified with tools; using 'basic-tools' instead.",
                    style("Note:").yellow()
                );
                return Ok(AgentTemplate::BasicTools);
            }
            AgentTemplate::Coordinator if !tool_ids.is_empty() => {
                println!(
                    "{} Coordinator template uses system tools; ignoring specified tools.",
                    style("Note:").yellow()
                );
            }
            _ => {}
        }
        return Ok(t);
    }

    // Invalid template name
    bail!(
        "Unknown template '{}'. Valid templates: simple, basic-tools, planner, coordinator",
        template_str
    );
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

/// Print dry-run summary.
fn print_dry_run_summary(
    slug: &str,
    tool_ids: &[String],
    template: AgentTemplate,
    description: &str,
    output_dir: &Path,
) {
    println!(
        "{}",
        style("Dry run - no files will be created").yellow().bold()
    );
    println!();
    println!("Would create agent:");
    println!("  Name: {}", style(slug).cyan());
    println!("  Template: {}", style(template.name()).green());
    println!(
        "  Description: {}",
        if description.is_empty() {
            "(none)"
        } else {
            description
        }
    );
    println!(
        "  Tools: {}",
        if tool_ids.is_empty() {
            "(none)".to_string()
        } else {
            tool_ids.join(", ")
        }
    );
    println!("  Output: {}", style(output_dir.display()).cyan());
    println!();
    println!("Files that would be created:");
    println!("  {}/", output_dir.display());
    println!("    manifest.json");
    println!("    tsconfig.json");
    println!("    baml_src/");
    println!("      {}_prompt.baml", slug.replace('-', "_"));
    println!("      generated_tools.baml (after type generation)");
    println!("    src/");
    println!("      index.ts");
    println!("      baml-runtime.d.ts (after type generation)");
}

/// Create a basic agent (simple or basic-tools) using run_bootstrap.
fn create_basic_agent(
    output_dir: &Path,
    name: &str,
    description: &str,
    tool_ids: &[String],
) -> Result<()> {
    // Use tokio runtime to run async bootstrap
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        baml_rt_builder::builder::bootstrap::run_bootstrap(output_dir, name, description, tool_ids)
            .await
            .map_err(|e| anyhow::anyhow!("Bootstrap failed: {}", e))
    })
}

/// Create a planner-style agent (3-phase: Intent -> Plan -> Execute).
fn create_planner_agent(
    output_dir: &Path,
    name: &str,
    description: &str,
    tool_ids: &[String],
) -> Result<()> {
    let slug = name.to_string();
    let prompt_name = slug.replace('-', "_");

    // Create directory structure
    std::fs::create_dir_all(output_dir.join("baml_src"))?;
    std::fs::create_dir_all(output_dir.join("src"))?;

    // Generate manifest.json
    let manifest = agent_planner::generate_manifest(&slug, description, tool_ids);
    std::fs::write(output_dir.join("manifest.json"), manifest)?;

    // Generate BAML prompt file
    let baml_prompt = agent_planner::generate_baml_prompt(&prompt_name, tool_ids);
    std::fs::write(
        output_dir
            .join("baml_src")
            .join(format!("{}_prompt.baml", prompt_name)),
        baml_prompt,
    )?;

    // Generate index.ts
    let index_ts = agent_planner::generate_index_ts(&prompt_name, tool_ids);
    std::fs::write(output_dir.join("src").join("index.ts"), index_ts)?;

    // Generate tsconfig.json
    let tsconfig = agent_simple::generate_tsconfig();
    std::fs::write(output_dir.join("tsconfig.json"), tsconfig)?;

    Ok(())
}

/// Create a coordinator-style agent (multi-agent delegator).
fn create_coordinator_agent(output_dir: &Path, name: &str, description: &str) -> Result<()> {
    let slug = name.to_string();
    let prompt_name = slug.replace('-', "_");

    // Coordinator always uses these system tools
    let tool_ids = vec![
        "system/discover_agents".to_string(),
        "system/discover_tools".to_string(),
        "system/internal_a2a".to_string(),
    ];

    // Create directory structure
    std::fs::create_dir_all(output_dir.join("baml_src"))?;
    std::fs::create_dir_all(output_dir.join("src"))?;

    // Generate manifest.json
    let manifest = agent_coordinator::generate_manifest(&slug, description, &tool_ids);
    std::fs::write(output_dir.join("manifest.json"), manifest)?;

    // Generate planner.baml
    let planner_baml = agent_coordinator::generate_planner_baml(&prompt_name);
    std::fs::write(
        output_dir.join("baml_src").join("planner.baml"),
        planner_baml,
    )?;

    // Generate coordinator prompt BAML
    let coordinator_baml = agent_coordinator::generate_coordinator_baml(&prompt_name);
    std::fs::write(
        output_dir
            .join("baml_src")
            .join(format!("{}_prompt.baml", prompt_name)),
        coordinator_baml,
    )?;

    // Generate index.ts
    let index_ts = agent_coordinator::generate_index_ts(&prompt_name);
    std::fs::write(output_dir.join("src").join("index.ts"), index_ts)?;

    // Generate tsconfig.json
    let tsconfig = agent_simple::generate_tsconfig();
    std::fs::write(output_dir.join("tsconfig.json"), tsconfig)?;

    Ok(())
}

/// Run type generation for the agent.
fn run_type_generation(output_dir: &Path) -> Result<()> {
    use baml_rt_builder::builder::{
        AgentDir, BuildDir, RuntimeTypeGenerator, compiler::write_canonical_tsconfig,
        traits::TypeGenerator,
    };

    // Ensure canonical tsconfig
    write_canonical_tsconfig(output_dir).context("Failed to write tsconfig.json")?;

    let agent_dir = AgentDir::new(output_dir.to_path_buf()).context("Failed to create AgentDir")?;
    let build_dir = BuildDir::new().context("Failed to create BuildDir")?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let generator = RuntimeTypeGenerator::new();
        generator
            .generate(&agent_dir, &build_dir)
            .await
            .map_err(|e| anyhow::anyhow!("Type generation failed: {}", e))?;

        // Sync generated BAML files
        sync_generated_baml_files(&build_dir, &agent_dir.baml_src())
    })
}

/// Sync generated_*.baml files from build_dir to agent's baml_src.
fn sync_generated_baml_files(
    build_dir: &baml_rt_builder::builder::BuildDir,
    dest_baml_src: &Path,
) -> Result<()> {
    use std::io::Write;

    let generated_src_dir = build_dir.join("baml_src");
    if !generated_src_dir.is_dir() {
        // No generated files to sync
        return Ok(());
    }

    std::fs::create_dir_all(dest_baml_src)?;

    let mut generated_names: std::collections::HashSet<String> = std::collections::HashSet::new();
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

    // Remove stale generated_*.baml files
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
