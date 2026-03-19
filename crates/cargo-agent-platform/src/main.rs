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
//! - `list-tools` — List all registered tools from the inventory
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

        /// Access level: read, write, or delete
        #[arg(long, default_value = "read")]
        access: String,

        /// Print what would be created/modified without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// List all registered tools from the inventory
    ListTools,

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

        Commands::ListTools => commands::list_tools::run(),

        Commands::Doctor {
            ci,
            warn_missing_catalog,
        } => commands::doctor::run(ci, warn_missing_catalog),
    }
}
