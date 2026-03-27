//! `cargo-agent-platform` CLI for scaffolding BAML tools and agents.
//!
// Allow unexpected cfg values from the force_link_all_tools! macro - these features
// are passed through from baml-tool-links and checked at compile time there.
#![allow(unexpected_cfgs)]
//!
//! Invoked as `cargo agent-platform <subcommand>`.
//!
//! # Subcommands
//!
//! - `new-tool <name>` — Create a new tool crate with all necessary patches
//! - `new-agent <name>` — Create a new agent package (supports `--subscriptions` for event delivery)
//! - `build [name]` — Package an agent into a distributable tar.gz
//! - `publish --agent-dir <path>` — Publish source bundle to repository
//! - `deploy --hash <hash>` — Activate a deployed hash in a running runner
//! - `undeploy --hash <hash>` — Remove an active deployed hash from a running runner
//! - `list-deployed-instances` — List loaded agent instances from a running runner
//! - `list-tools` — List all registered tools from the inventory
//! - `list-agents` — List all agent packages
//! - `list-event-sources` — List event source kinds declared by tools and known schema versions
//! - `regen` — Regenerate type declarations for all agents
//! - `doctor` — Validate workspace integrity

mod commands;
mod generated_baml;
mod interactive;
mod patchers;
mod templates;
mod text;
mod tool_catalog;
mod transaction;
mod workspace;

use clap::{Parser, Subcommand};
use commands::publish::PublishOriginArg;

/// Agent Platform SDK CLI
///
/// Scaffolds new tools and agents for the BAML agent platform.
/// Invoked as `cargo agent-platform <subcommand>`.
#[derive(Parser)]
#[command(
    name = "cargo-agent-platform",
    bin_name = "cargo agent-platform",
    version,
    about = "Agent Platform SDK CLI for scaffolding tools and agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new tool crate with all necessary patches
    NewTool {
        /// Tool name in kebab-case (e.g., github, jira, linear). Omit for interactive mode.
        name: Option<String>,

        /// Bundle type (only 'support' is currently supported)
        #[arg(long)]
        bundle: Option<String>,

        /// Access level: read (default, query-only) or write (can mutate)
        #[arg(long)]
        access: Option<String>,

        /// Human-readable description for this tool
        #[arg(long)]
        description: Option<String>,

        /// Validate and print planned changes without writing files; exits non-zero on validation failures (non-interactive only)
        #[arg(long)]
        dry_run: bool,
    },

    /// Create a new agent package
    NewAgent {
        /// Agent name in kebab-case (e.g., github-agent, task-manager). Omit for interactive mode.
        name: Option<String>,

        /// Comma-separated tool IDs (e.g., support/github,system/internal_a2a)
        #[arg(long)]
        tools: Option<String>,

        /// Agent template: simple, basic-tools, planner, coordinator
        #[arg(long)]
        template: Option<String>,

        /// Human-readable description for discovery
        #[arg(long)]
        description: Option<String>,

        /// Comma-separated manifest tags (e.g., support,clickup,prod)
        #[arg(long)]
        tags: Option<String>,

        /// Event subscriptions for receiving dispatched events.
        /// Format: "schema=<version>,sources=<kind1,kind2>"
        /// Example: --subscriptions "schema=task-daemon.interpretation.v1,sources=slack,clickup"
        #[arg(long)]
        subscriptions: Option<String>,

        /// Target directory (defaults to agents/<name>)
        #[arg(long)]
        output: Option<String>,

        /// Validate and print planned changes without writing files; exits non-zero on validation failures (non-interactive only)
        #[arg(long)]
        dry_run: bool,
    },

    /// List all registered tools from the inventory
    ListTools,

    /// List all agent packages
    ListAgents,

    /// List event source kinds declared by tools and known schema versions
    ListEventSources,

    /// Package agents into distributable tar.gz files
    Build {
        /// Agent names (looks in agents/ directory). Omit to build current directory.
        #[arg()]
        names: Vec<String>,

        /// Explicit path to agent directory (overrides name lookup, only valid with single agent)
        #[arg(long)]
        path: Option<String>,

        /// Output directory for tar.gz files (default: current directory)
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Publish an agent source bundle to repository
    Publish {
        /// Path to an agent source directory (contains manifest.json + baml_src/)
        #[arg(long, default_value = ".")]
        agent_dir: String,

        /// Repository base URL where repository routes are mounted
        #[arg(long, default_value = "http://127.0.0.1:8080/repository")]
        repository_url: String,

        /// Why this publish happened
        #[arg(long, default_value = "published from source directory")]
        rationale: String,

        /// Publish origin kind: original | iteration
        #[arg(long, value_enum, default_value_t = PublishOriginArg::Iteration)]
        origin: PublishOriginArg,
    },

    /// Deploy an agent artifact by content hash into a running runner
    Deploy {
        /// Content hash returned by repository publish
        #[arg(long)]
        hash: String,

        /// Runner base URL (without /repository)
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        url: String,
    },

    /// Undeploy an active agent package by content hash from a running runner
    Undeploy {
        /// Content hash of the deployed package
        #[arg(long)]
        hash: String,

        /// Runner base URL (without /repository)
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        url: String,
    },

    /// List deployed agent instances from a running runner
    ListDeployedInstances {
        /// Runner base URL (without /repository)
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        url: String,
    },

    /// Regenerate generated_tools.baml and baml-runtime.d.ts for all agents
    Regen {
        /// Agent names to regenerate (omit for all agents)
        #[arg()]
        names: Vec<String>,
    },

    /// Validate workspace integrity
    Doctor {
        /// Exit non-zero on any issue (for CI)
        #[arg(long)]
        ci: bool,

        /// Downgrade missing catalog entries from error to warning
        #[arg(long)]
        warn_missing_catalog: bool,
    },

    /// Interactive chat with a deployed agent
    Chat {
        /// Agent name (package name from manifest)
        #[arg(long)]
        agent: String,

        /// Runner base URL
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        url: String,

        /// Instance identifier (default: "default")
        #[arg(long, default_value = "default")]
        instance: String,

        /// Enable verbose output (show SSE events, request details)
        #[arg(long, short)]
        verbose: bool,
    },
}

fn main() -> anyhow::Result<()> {
    // Force-link all tools so the inventory is complete
    baml_tool_links::force_link_all_tools!();

    // When invoked as `cargo agent-platform`, Cargo passes "agent-platform" as the
    // first argument. Strip it so clap sees the actual command/flags.
    let args: Vec<String> = std::env::args()
        .enumerate()
        .filter(|(i, arg)| !(*i == 1 && arg == "agent-platform"))
        .map(|(_, arg)| arg)
        .collect();

    let cli = Cli::parse_from(args);

    match cli.command {
        Commands::NewTool {
            name,
            bundle,
            access,
            description,
            dry_run,
        } => {
            // Interactive mode when name is not provided
            let interactive = name.is_none();

            let name = match name {
                Some(n) => n,
                None => interactive::prompt_tool_name()?,
            };

            let bundle = match bundle {
                Some(b) => b,
                None if interactive => interactive::prompt_bundle()?,
                None => "support".to_string(),
            };

            let access = match access {
                Some(a) => a,
                None if interactive => interactive::prompt_access()?,
                None => "read".to_string(),
            };

            let description = match description {
                Some(d) => d,
                None if interactive => interactive::prompt_tool_description()?,
                None => String::new(),
            };

            // In interactive mode, ignore --dry-run (confirmation prompt replaces it)
            let dry_run = if interactive { false } else { dry_run };

            commands::new_tool::run(&name, &bundle, &access, &description, dry_run, interactive)
        }

        Commands::NewAgent {
            name,
            tools,
            template,
            description,
            tags,
            subscriptions,
            output,
            dry_run,
        } => {
            // Interactive mode when name is not provided
            let interactive = name.is_none();

            let name = match name {
                Some(n) => n,
                None => interactive::prompt_agent_name()?,
            };

            let description = match description {
                Some(d) => d,
                None if interactive => interactive::prompt_agent_description()?,
                None => String::new(),
            };

            let template = match template {
                Some(t) => t,
                None if interactive => interactive::prompt_template()?,
                None => "simple".to_string(),
            };

            // For interactive mode with basic-tools or planner, prompt for tools
            let tools = match tools {
                Some(t) => Some(t),
                None if interactive && (template == "basic-tools" || template == "planner") => {
                    interactive::prompt_tools()?
                }
                None => None,
            };

            // Parse tool IDs for subscription prompting
            let tool_ids: Vec<String> = tools
                .as_ref()
                .map(|t| {
                    t.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();

            let suggested_tags = if interactive {
                interactive::suggest_agent_tags(&tool_ids)?
            } else {
                Vec::new()
            };

            let tags = match tags {
                Some(t) => Some(t),
                None if interactive => interactive::prompt_agent_tags(&suggested_tags)?,
                None => None,
            };

            // Handle subscriptions: from CLI flag or interactive prompt
            let subscriptions = match subscriptions {
                Some(s) => Some(s),
                None if interactive => interactive::prompt_subscriptions(&tool_ids)?,
                None => None,
            };

            // In interactive mode, ignore --dry-run (confirmation prompt replaces it)
            let dry_run = if interactive { false } else { dry_run };

            commands::new_agent::run(
                &name,
                tools.as_deref(),
                &template,
                &description,
                tags.as_deref(),
                subscriptions.as_deref(),
                output.as_deref(),
                dry_run,
                interactive,
            )
        }

        Commands::ListTools => commands::list_tools::run(),

        Commands::ListAgents => commands::list_agents::run(),

        Commands::ListEventSources => commands::list_event_sources::run(),

        Commands::Build {
            names,
            path,
            output,
        } => commands::build::run(&names, path.as_deref(), output.as_deref()),

        Commands::Publish {
            agent_dir,
            repository_url,
            rationale,
            origin,
        } => commands::publish::run(&agent_dir, &repository_url, &rationale, origin),

        Commands::Deploy { hash, url } => commands::deploy::run(&hash, &url),

        Commands::Undeploy { hash, url } => commands::undeploy::run(&hash, &url),

        Commands::ListDeployedInstances { url } => commands::list_deployed_instances::run(&url),

        Commands::Regen { names } => commands::regen::run(&names),

        Commands::Doctor {
            ci,
            warn_missing_catalog,
        } => commands::doctor::run(ci, warn_missing_catalog),

        Commands::Chat {
            agent,
            url,
            instance,
            verbose,
        } => commands::chat::run(&agent, &url, &instance, verbose),
    }
}
