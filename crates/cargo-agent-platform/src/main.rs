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
//! - `new-agent <name>` — Create a new agent package
//! - `list-tools` — List all registered tools from the inventory
//! - `list-agents` — List all agent packages
//! - `regen` — Regenerate type declarations for all agents
//! - `doctor` — Validate workspace integrity

mod commands;
mod patchers;
mod templates;
mod transaction;

use clap::{Parser, Subcommand};

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
        /// Tool name in kebab-case (e.g., github, jira, linear)
        name: String,

        /// Bundle type (only 'support' is currently supported)
        #[arg(long, default_value = "support")]
        bundle: String,

        /// Access level: read (default, query-only) or write (can mutate)
        #[arg(long, default_value = "read")]
        access: String,

        /// Print what would be created/modified without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Create a new agent package
    NewAgent {
        /// Agent name in kebab-case (e.g., github-agent, task-manager)
        name: String,

        /// Comma-separated tool IDs (e.g., support/github,system/internal_a2a)
        #[arg(long)]
        tools: Option<String>,

        /// Agent template: simple, basic-tools, planner, coordinator
        #[arg(long, default_value = "simple")]
        template: String,

        /// Human-readable description for discovery
        #[arg(long, default_value = "")]
        description: String,

        /// Target directory (defaults to agents/<name>)
        #[arg(long)]
        output: Option<String>,

        /// Print what would be created without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// List all registered tools from the inventory
    ListTools,

    /// List all agent packages
    ListAgents,

    /// Regenerate generated_tools.baml and baml-runtime.d.ts for all agents
    Regen,

    /// Validate workspace integrity
    Doctor {
        /// Exit non-zero on any issue (for CI)
        #[arg(long)]
        ci: bool,

        /// Downgrade missing catalog entries from error to warning
        #[arg(long)]
        warn_missing_catalog: bool,
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
            dry_run,
        } => commands::new_tool::run(&name, &bundle, &access, dry_run),

        Commands::NewAgent {
            name,
            tools,
            template,
            description,
            output,
            dry_run,
        } => commands::new_agent::run(
            &name,
            tools.as_deref(),
            &template,
            &description,
            output.as_deref(),
            dry_run,
        ),

        Commands::ListTools => commands::list_tools::run(),

        Commands::ListAgents => commands::list_agents::run(),

        Commands::Regen => commands::regen::run(),

        Commands::Doctor {
            ci,
            warn_missing_catalog,
        } => commands::doctor::run(ci, warn_missing_catalog),
    }
}
