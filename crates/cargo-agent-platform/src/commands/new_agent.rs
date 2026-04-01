//! `new-agent` subcommand — create a new agent package.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_core::{EventSchemaVersion, EventSourceKind, EventSubscription};
use console::style;

use crate::{
    generated_baml::sync_generated_baml_files,
    templates::{agent_coordinator, agent_planner, agent_simple},
    tool_catalog::canonicalize_tool_ids,
    workspace::find_workspace_root,
};

const BANNED_AGENT_TAGS: &[&str] = &["support", "read", "write", "system"];

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

fn validate_template_subscriptions(
    template: AgentTemplate,
    subscriptions: &[EventSubscription],
) -> Result<()> {
    if template == AgentTemplate::Coordinator && !subscriptions.is_empty() {
        bail!(
            "Error: coordinator template does not support `--subscriptions` yet.\nHint: coordinators currently scaffold conversational orchestration only; use a non-coordinator template and add `onDispatch` manually, or add dispatch support before enabling coordinator subscriptions."
        );
    }
    Ok(())
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
#[allow(clippy::too_many_arguments)]
pub fn run(
    name: &str,
    tools: Option<&str>,
    template: &str,
    description: &str,
    tags: Option<&str>,
    subscriptions: Option<&str>,
    output: Option<&str>,
    dry_run: bool,
    interactive: bool,
) -> Result<()> {
    // Validate agent name
    let slug = validate_agent_name(name)?;

    // Parse tools
    let raw_tool_ids: Vec<String> = match tools {
        Some(t) if !t.trim().is_empty() => t
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let tool_ids = canonicalize_tool_ids(&raw_tool_ids);
    if tool_ids.len() < raw_tool_ids.len() {
        println!(
            "{} Collapsed overlapping tool variants to canonical IDs: {}",
            style("Note:").yellow(),
            tool_ids.join(", ")
        );
    }

    // Parse tags
    let tags = parse_csv_list(tags);
    validate_tags(&tags)?;

    // Parse subscriptions
    let subscriptions = match subscriptions {
        Some(s) => parse_subscriptions(s)?,
        None => Vec::new(),
    };

    // Determine template
    let template = determine_template(template, &tool_ids)?;
    validate_template_subscriptions(template, &subscriptions)?;

    // Find workspace root and determine output directory
    let workspace_root = find_workspace_root()?;
    let output_dir = match output {
        Some(p) => PathBuf::from(p),
        None => workspace_root.join("agents").join(&slug),
    };

    // Validate output directory state (existing non-empty directory is always an error).
    validate_output_dir(&output_dir)?;

    // Show summary for interactive or dry-run mode
    if interactive || dry_run {
        print_summary(
            &slug,
            &tool_ids,
            template,
            description,
            &tags,
            &subscriptions,
            &output_dir,
        );
    }

    if dry_run {
        println!();
        println!(
            "{}",
            style("Dry run successful - validation passed, no changes made.").yellow()
        );
        return Ok(());
    }

    // In interactive mode, ask for confirmation
    if interactive {
        println!();
        if !crate::interactive::confirm_proceed()? {
            println!("{}", style("Aborted - no changes made.").yellow());
            return Ok(());
        }
    }

    // Create agent based on template
    println!();
    println!(
        "{} Creating agent '{}' with template '{}'...",
        style("[1/2]").bold().dim(),
        style(&slug).cyan(),
        style(template.name()).green()
    );

    match template {
        AgentTemplate::Simple | AgentTemplate::BasicTools => {
            create_basic_agent(
                &output_dir,
                &slug,
                description,
                &tags,
                &tool_ids,
                &subscriptions,
            )
                .map_err(|e| {
                    anyhow!(
                        "Error: failed to scaffold agent files.\nCause: {e}\nHint: check the output directory and workspace configuration, then retry with `--dry-run`."
                    )
                })?;
        }
        AgentTemplate::Planner => {
            create_planner_agent(
                &output_dir,
                &slug,
                description,
                &tags,
                &tool_ids,
                &subscriptions,
            )
            .map_err(|e| {
                anyhow!(
                    "Error: failed to scaffold planner template files.\nCause: {e}\nHint: verify write permissions for `{}`.",
                    output_dir.display()
                )
            })?;
        }
        AgentTemplate::Coordinator => {
            create_coordinator_agent(&output_dir, &slug, description, &tags, &subscriptions)
                .map_err(|e| {
                    anyhow!(
                        "Error: failed to scaffold coordinator template files.\nCause: {e}\nHint: verify write permissions for `{}`.",
                        output_dir.display()
                    )
                })?;
        }
    }

    println!("{} Running type generation...", style("[2/2]").bold().dim());

    // Run type generation
    run_type_generation(&output_dir).map_err(|e| {
        anyhow!(
            "Error: type generation failed for agent at {}.\nCause: {e}\nHint: run with `--dry-run` first to validate inputs, then inspect BAML/template files for errors.",
            output_dir.display()
        )
    })?;

    println!();
    println!("{}", style("Agent created successfully!").green().bold());
    println!();
    println!("  Location: {}", style(output_dir.display()).cyan());
    if !subscriptions.is_empty() {
        println!(
            "  {}",
            style("Event subscriptions configured in manifest.json").dim()
        );
    }
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

/// Validate output directory can be used for agent creation.
fn validate_output_dir(output_dir: &Path) -> Result<()> {
    if output_dir.exists() {
        let entries: Vec<std::fs::DirEntry> =
            std::fs::read_dir(output_dir)?.collect::<Result<_, _>>()?;
        if !entries.is_empty() {
            bail!(
                "Error: output directory already exists and is non-empty: {}\nHint: pass `--output <new-dir>` or clean the directory before retrying.",
                output_dir.display()
            );
        }
    }
    Ok(())
}

/// Parse subscriptions from CLI format:
/// "schema=<version1,version2>,sources=<kind1,kind2>"
///
/// Examples:
/// - "schema=task-daemon.interpretation.v1,sources=slack,clickup"
/// - "schema=task-daemon.interpretation.v1,custom.schema.v2,sources=slack"
fn parse_subscriptions(input: &str) -> Result<Vec<EventSubscription>> {
    let mut schema_versions = Vec::new();
    let mut source_kinds = Vec::new();
    let mut saw_schema_key = false;
    let mut saw_sources_key = false;

    // Top-level key/value list where values themselves can also contain commas.
    // We find key boundaries first, then split each value list.
    let mut remaining = input.trim();

    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix("schema=") {
            saw_schema_key = true;
            // Find the end of the schema value (next key= or end of string)
            let end_pos = rest
                .find(",sources=")
                .or_else(|| rest.find(",schema="))
                .unwrap_or(rest.len());
            let schema_value = &rest[..end_pos];

            // Parse comma-separated schema versions.
            for schema in schema_value.split(',') {
                let schema = schema.trim();
                if !schema.is_empty()
                    && let Some(sv) = EventSchemaVersion::parse(schema)
                {
                    schema_versions.push(sv);
                }
            }

            remaining = if end_pos < rest.len() {
                rest[end_pos..].trim_start_matches(',')
            } else {
                ""
            };
        } else if let Some(rest) = remaining.strip_prefix("sources=") {
            saw_sources_key = true;
            // Find the end of sources value
            let end_pos = rest.find(",schema=").unwrap_or(rest.len());
            let sources_value = &rest[..end_pos];

            // Parse comma-separated source kinds
            for source in sources_value.split(',') {
                let source = source.trim();
                if !source.is_empty()
                    && !source.starts_with("schema=")
                    && let Some(sk) = EventSourceKind::parse(source)
                {
                    source_kinds.push(sk);
                }
            }

            remaining = if end_pos < rest.len() {
                rest[end_pos..].trim_start_matches(',')
            } else {
                ""
            };
        } else {
            // Unknown key, skip to next comma
            let next_comma = remaining.find(',').unwrap_or(remaining.len());
            remaining = if next_comma < remaining.len() {
                &remaining[next_comma + 1..]
            } else {
                ""
            };
        }
    }

    if saw_schema_key && schema_versions.is_empty() {
        bail!(
            "Error: `schema=` was provided but no schema versions were parsed.\nHint: use `schema=<version>` or `schema=<version1,version2>`."
        );
    }

    if saw_sources_key && source_kinds.is_empty() {
        bail!(
            "Error: `sources=` was provided but no source kinds were parsed.\nHint: use `sources=<kind1,kind2>`."
        );
    }

    if schema_versions.is_empty() && source_kinds.is_empty() {
        bail!(
            "Error: invalid subscription format.\nHint: expected `schema=<version>,sources=<kind1,kind2>`.\nExample: --subscriptions \"schema=task-daemon.interpretation.v1,sources=slack,clickup\""
        );
    }

    Ok(vec![EventSubscription {
        schema_versions,
        source_kinds,
        source_keys: Vec::new(),
        source_key_prefixes: Vec::new(),
    }])
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
        bail!(
            "Error: agent name must contain at least one alphanumeric character.\nHint: use names like `github-agent` or `intake-agent`."
        );
    }

    // Check for reserved names
    let reserved = ["test", "lib", "bin", "build", "dev", "src", "agents"];
    if reserved.contains(&slug.as_str()) {
        bail!(
            "Error: '{}' is a reserved name and cannot be used as an agent name.\nHint: choose a different slug, for example `my-{}-agent`.",
            slug,
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
        "Error: unknown template '{}'.\nHint: valid templates are: simple, basic-tools, planner, coordinator.",
        template_str
    );
}

/// Print summary of what will be created.
fn print_summary(
    slug: &str,
    tool_ids: &[String],
    template: AgentTemplate,
    description: &str,
    tags: &[String],
    subscriptions: &[EventSubscription],
    output_dir: &Path,
) {
    println!();
    println!("{}", style("Summary:").bold());
    println!("  Name:        {}", style(slug).cyan());
    println!("  Template:    {}", style(template.name()).green());
    println!(
        "  Description: {}",
        if description.is_empty() {
            "(none)".to_string()
        } else {
            description.to_string()
        }
    );
    println!(
        "  Tools:       {}",
        if tool_ids.is_empty() {
            "(none)".to_string()
        } else {
            tool_ids.join(", ")
        }
    );
    println!(
        "  Tags:        {}",
        if tags.is_empty() {
            "(none)".to_string()
        } else {
            tags.join(", ")
        }
    );

    // Display subscriptions
    if subscriptions.is_empty() {
        println!("  Subscriptions: (none)");
    } else {
        println!("  Subscriptions:");
        for sub in subscriptions {
            if !sub.schema_versions.is_empty() {
                let schemas: Vec<_> = sub.schema_versions.iter().map(|s| s.as_str()).collect();
                println!("    Schemas: {}", schemas.join(", "));
            }
            if !sub.source_kinds.is_empty() {
                let sources: Vec<_> = sub.source_kinds.iter().map(|s| s.as_str()).collect();
                println!("    Sources: {}", sources.join(", "));
            }
        }
    }

    println!("  Output:      {}", style(output_dir.display()).cyan());
    println!();
    println!("{}", style("Files to be created:").bold());
    println!("  {}/", output_dir.display());
    println!("    manifest.json");
    println!("    tsconfig.json");
    println!("    baml_src/");
    println!("      {}_prompt.baml", slug.replace('-', "_"));
    println!("      _baml_runtime.baml (after type generation)");
    println!("    src/");
    println!("      index.ts");
    println!("      baml-runtime.d.ts (after type generation)");
}

/// Create a basic agent (simple or basic-tools).
fn create_basic_agent(
    output_dir: &Path,
    name: &str,
    description: &str,
    tags: &[String],
    tool_ids: &[String],
    subscriptions: &[EventSubscription],
) -> Result<()> {
    let slug = name.to_string();
    let prompt_name = slug.replace('-', "_");

    std::fs::create_dir_all(output_dir.join("baml_src"))?;
    std::fs::create_dir_all(output_dir.join("src"))?;

    let manifest =
        agent_simple::generate_manifest(&slug, description, tags, tool_ids, subscriptions);
    std::fs::write(output_dir.join("manifest.json"), manifest)?;

    let baml_prompt = agent_simple::generate_baml_prompt(&prompt_name, tool_ids);
    std::fs::write(
        output_dir
            .join("baml_src")
            .join(format!("{}_prompt.baml", prompt_name)),
        baml_prompt,
    )?;

    let index_ts = agent_simple::generate_index_ts(&prompt_name, !tool_ids.is_empty());
    std::fs::write(output_dir.join("src").join("index.ts"), index_ts)?;

    let tsconfig = agent_simple::generate_tsconfig();
    std::fs::write(output_dir.join("tsconfig.json"), tsconfig)?;

    Ok(())
}

/// Create a planner-style agent (3-phase: Intent -> Plan -> Execute).
fn create_planner_agent(
    output_dir: &Path,
    name: &str,
    description: &str,
    tags: &[String],
    tool_ids: &[String],
    subscriptions: &[EventSubscription],
) -> Result<()> {
    let slug = name.to_string();
    let prompt_name = slug.replace('-', "_");

    // Create directory structure
    std::fs::create_dir_all(output_dir.join("baml_src"))?;
    std::fs::create_dir_all(output_dir.join("src"))?;

    // Generate manifest.json (with subscriptions)
    let manifest =
        agent_planner::generate_manifest(&slug, description, tags, tool_ids, subscriptions);
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
fn create_coordinator_agent(
    output_dir: &Path,
    name: &str,
    description: &str,
    tags: &[String],
    _subscriptions: &[EventSubscription],
) -> Result<()> {
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

    // Generate manifest.json. Coordinators are orchestration-only until the
    // template grows a real onDispatch path.
    let manifest = agent_coordinator::generate_manifest(&slug, description, tags, &tool_ids);
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
    let index_ts = agent_coordinator::generate_index_ts(&slug);
    std::fs::write(output_dir.join("src").join("index.ts"), index_ts)?;

    // Generate tsconfig.json
    let tsconfig = agent_simple::generate_tsconfig();
    std::fs::write(output_dir.join("tsconfig.json"), tsconfig)?;

    Ok(())
}

fn parse_csv_list(input: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    for item in input.unwrap_or("").split(',') {
        let trimmed = item.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        if out.iter().any(|existing: &String| existing == &trimmed) {
            continue;
        }
        out.push(trimmed);
    }
    out
}

fn validate_tags(tags: &[String]) -> Result<()> {
    if tags.is_empty() {
        bail!(
            "Error: tags cannot be empty.\nHint: provide at least one specific tag with `--tags` (comma-separated), or enter tags in interactive mode."
        );
    }

    let banned_found: Vec<String> = tags
        .iter()
        .filter(|tag| BANNED_AGENT_TAGS.contains(&tag.as_str()))
        .cloned()
        .collect();
    if !banned_found.is_empty() {
        bail!(
            "Error: tags contain banned generic values: {}.\nHint: remove generic tags (`support`, `read`, `write`, `system`) and use feature/domain tags instead.",
            banned_found.join(", ")
        );
    }

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
    let generate_result = rt.block_on(async {
        let generator = RuntimeTypeGenerator::new();
        generator
            .generate(&agent_dir, &build_dir)
            .await
            .map_err(|e| anyhow::anyhow!("Type generation failed: {}", e))?;

        // Sync generated BAML files
        sync_generated_baml_files(&build_dir, &agent_dir.baml_src())
    });

    if let Err(err) = generate_result {
        let msg = err.to_string();
        if msg.contains("Tool metadata missing for:") {
            println!(
                "{} Missing in-process tool metadata; falling back to workspace-local regen...",
                style("Note:").yellow()
            );
            run_local_regen_fallback(output_dir)?;
            return Ok(());
        }
        return Err(err);
    }

    Ok(())
}

fn run_local_regen_fallback(output_dir: &Path) -> Result<()> {
    let workspace_root = find_workspace_root()?;
    let agents_root = workspace_root.join("agents");

    // Prefer targeted regen when output_dir is workspace/agents/<name>.
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&workspace_root)
        .arg("run")
        .arg("-p")
        .arg("cargo-agent-platform")
        .arg("--")
        .arg("regen");

    let target_name = output_dir
        .strip_prefix(&agents_root)
        .ok()
        .and_then(|p| p.components().next())
        .and_then(|c| c.as_os_str().to_str())
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string());

    if let Some(name) = target_name {
        cmd.arg(name);
    }

    let status = cmd.status().context(
        "Failed to launch local `cargo run -p cargo-agent-platform -- regen` fallback command",
    )?;

    if status.success() {
        Ok(())
    } else {
        bail!(
            "Fallback regen command failed with status {status}. \
Hint: run `cargo run -p cargo-agent-platform -- regen` manually from the workspace root."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentTemplate, parse_subscriptions, validate_output_dir, validate_template_subscriptions,
    };

    #[test]
    fn validate_output_dir_allows_missing_or_empty_dir() {
        let temp = tempfile::TempDir::new().expect("tempdir");

        let missing = temp.path().join("missing");
        assert!(validate_output_dir(&missing).is_ok());

        let empty = temp.path().join("empty");
        std::fs::create_dir_all(&empty).expect("create empty dir");
        assert!(validate_output_dir(&empty).is_ok());
    }

    #[test]
    fn validate_output_dir_rejects_non_empty_dir() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let dir = temp.path().join("non-empty");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("marker.txt"), "x").expect("write marker");

        assert!(validate_output_dir(&dir).is_err());
    }

    #[test]
    fn parse_subscriptions_single_schema_and_sources() {
        let parsed = parse_subscriptions("schema=task-daemon.interpretation.v1,sources=slack")
            .expect("should parse");

        assert_eq!(parsed.len(), 1);
        let sub = &parsed[0];
        assert_eq!(
            sub.schema_versions
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec!["task-daemon.interpretation.v1"]
        );
        assert_eq!(
            sub.source_kinds
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec!["slack"]
        );
    }

    #[test]
    fn parse_subscriptions_supports_multiple_schema_versions() {
        let parsed = parse_subscriptions(
            "schema=task-daemon.interpretation.v1,task-daemon.interpretation.v2,sources=slack,clickup",
        )
        .expect("should parse");

        assert_eq!(parsed.len(), 1);
        let sub = &parsed[0];
        assert_eq!(
            sub.schema_versions
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec![
                "task-daemon.interpretation.v1",
                "task-daemon.interpretation.v2"
            ]
        );
        assert_eq!(
            sub.source_kinds
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            vec!["slack", "clickup"]
        );
    }

    #[test]
    fn parse_subscriptions_rejects_empty_schema_value_when_schema_key_present() {
        let err = parse_subscriptions("schema=,sources=slack").expect_err("should fail");
        assert!(
            err.to_string().contains("`schema=` was provided"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_subscriptions_rejects_empty_sources_value_when_sources_key_present() {
        let err = parse_subscriptions("schema=task-daemon.interpretation.v1,sources=")
            .expect_err("should fail");
        assert!(
            err.to_string().contains("`sources=` was provided"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn coordinator_template_rejects_subscriptions() {
        let subscriptions =
            parse_subscriptions("schema=task-daemon.interpretation.v1,sources=slack")
                .expect("should parse");
        let err = validate_template_subscriptions(AgentTemplate::Coordinator, &subscriptions)
            .expect_err("coordinator subscriptions should fail");
        assert!(
            err.to_string()
                .contains("does not support `--subscriptions`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn planner_template_allows_subscriptions() {
        let subscriptions =
            parse_subscriptions("schema=task-daemon.interpretation.v1,sources=slack")
                .expect("should parse");
        validate_template_subscriptions(AgentTemplate::Planner, &subscriptions)
            .expect("planner subscriptions should remain allowed");
    }
}
