// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Clap command definitions for `karak`.

use clap::{Parser, Subcommand};

use crate::commands::publish::PublishOriginArg;

/// Karakara — unified platform host and developer SDK
#[derive(Parser)]
#[command(name = "karak", version, about = "Karakara platform and developer SDK")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the Karakara platform (HTTP, stdio, repository, provenance)
    Serve {
        #[command(flatten)]
        runner: baml_agent_runner::RunnerCli,
    },

    /// Scaffold project config (`karakara.toml`) and optionally a first agent
    Init {
        #[arg(long, default_value = ".")]
        dir: String,

        #[arg(long, default_value = "http://127.0.0.1:18080")]
        runner_url: String,

        #[arg(long)]
        agent_name: Option<String>,

        #[arg(long)]
        with_agent: bool,
    },

    /// Inspect or update `karakara.toml`
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Publish source and deploy an agent, or enable an external tool
    Install {
        #[command(subcommand)]
        command: InstallCommands,
    },

    /// Pull server-generated BAML prelude and TypeScript stubs after publish
    SyncTypes {
        #[arg(long)]
        path: Option<String>,

        #[arg(long)]
        runner_token: Option<String>,
    },

    /// Bundled Cursor authoring skills
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },

    /// Eval harness (chat flows against a running runner)
    Eval {
        #[command(subcommand)]
        command: EvalCommands,
    },

    /// Author external tools (v2): scaffold, build, and test a TypeScript tool.
    Tool {
        #[command(subcommand)]
        command: ToolCommands,
    },

    /// Create a new *static* tool crate — compiled into the platform workspace.
    /// Use this only when extending the platform itself (e.g., adding a
    /// system-level bundle). For every other case, prefer `tool new`.
    NewStaticTool {
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

        /// Event subscriptions to record in manifest.json for dispatch-capable agents.
        /// Not supported for the `coordinator` template; other templates may still require a manual `onDispatch`.
        /// Format: "schema=<version>,sources=<kind1,kind2>"
        /// Example: --subscriptions "schema=host.source-records.v1,sources=slack"
        #[arg(long)]
        subscriptions: Option<String>,

        /// Repository URL used for interactive tool/source picker metadata. Wins over --snapshot-cache.
        #[arg(long)]
        repository_url: Option<String>,

        /// Read interactive tool/source picker metadata from unified offline snapshot cache.
        #[arg(long)]
        snapshot_cache: Option<String>,

        /// Target directory (defaults to agents/<name>)
        #[arg(long)]
        output: Option<String>,

        /// Validate and print planned changes without writing files; exits non-zero on validation failures (non-interactive only)
        #[arg(long)]
        dry_run: bool,
    },

    /// List tools from runner/repository catalog or offline snapshot cache
    ListTools {
        /// Repository URL to query when not using --snapshot-cache
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,

        /// Read tools from unified offline snapshot cache instead of repository
        #[arg(long)]
        snapshot_cache: Option<String>,
    },

    /// List all agent packages
    ListAgents,

    /// List event source kinds declared by runner/cache tools and known schema versions
    ListEventSources {
        /// Repository URL to query when not using --snapshot-cache
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,

        /// Read static tool catalog from unified offline snapshot cache instead of repository
        #[arg(long)]
        snapshot_cache: Option<String>,
    },

    /// Publish an agent source bundle to repository
    Publish {
        /// Path to an agent source directory (contains manifest.json + baml_src/)
        #[arg(long, default_value = ".")]
        agent_dir: String,

        /// Repository base URL where repository routes are mounted
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,

        /// Why this publish happened
        #[arg(long, default_value = "published from source directory")]
        rationale: String,

        /// Publish origin kind: original | iteration
        #[arg(long, value_enum, default_value_t = PublishOriginArg::Iteration)]
        origin: PublishOriginArg,

        /// Runner token for authenticated operator access (falls back to RUNNER_TOKEN env)
        #[arg(long)]
        runner_token: Option<String>,
    },

    /// Deploy an agent artifact by content hash into a running runner
    Deploy {
        /// Content hash returned by repository publish
        #[arg(long)]
        hash: String,

        /// Runner base URL (without /repository)
        #[arg(long, default_value = "http://127.0.0.1:18080")]
        url: String,

        /// Runner token for authenticated operator access (falls back to RUNNER_TOKEN env)
        #[arg(long)]
        runner_token: Option<String>,
    },

    /// Undeploy an active agent package by content hash from a running runner
    Undeploy {
        /// Content hash of the deployed package
        #[arg(long)]
        hash: String,

        /// Runner base URL (without /repository)
        #[arg(long, default_value = "http://127.0.0.1:18080")]
        url: String,

        /// Runner token for authenticated operator access (falls back to RUNNER_TOKEN env)
        #[arg(long)]
        runner_token: Option<String>,
    },

    /// List deployed agent instances from a running runner
    ListDeployedInstances {
        /// Runner base URL (without /repository)
        #[arg(long, default_value = "http://127.0.0.1:18080")]
        url: String,
    },

    /// Export all approved repository tool catalogs into an offline snapshot cache
    ExportSnapshotCache {
        /// Repository URL to export from
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,

        /// Output snapshot-cache directory
        #[arg(long, value_name = "DIR")]
        output: String,
    },

    /// Report contents of an explicit exported snapshot cache for offline CI
    SnapshotReport {
        /// Snapshot cache root (may contain mcp/ and external-tools/)
        #[arg(long)]
        snapshot_cache: String,

        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },

    /// Validate workspace integrity
    Doctor {
        /// Exit non-zero on any issue (for CI)
        #[arg(long)]
        ci: bool,

        /// Downgrade missing catalog entries from error to warning
        #[arg(long)]
        warn_missing_catalog: bool,

        /// Repository URL to query for catalog validation. Wins over --snapshot-cache.
        #[arg(long)]
        repository_url: Option<String>,

        /// Read tool catalog from unified offline snapshot cache instead of local inventory.
        #[arg(long)]
        snapshot_cache: Option<String>,
    },

    /// Inspect and manage external-tool snapshot cache entries
    ExternalTool {
        #[command(subcommand)]
        command: ExternalToolCommands,
    },

    /// Inspect and manage MCP registry entries
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },

    /// Interactive terminal chat with a deployed agent
    Chat {
        /// Agent package/name from discovery
        #[arg(long)]
        agent: String,

        /// Runner base URL
        #[arg(long, default_value = "http://127.0.0.1:18080")]
        url: String,

        /// Print debug diagnostics
        #[arg(long, short)]
        verbose: bool,
    },
}

/// `karak tool <new|build|test>` — the v2 external-tool authoring workflow.
#[derive(Subcommand)]
pub enum ToolCommands {
    /// Scaffold a new TypeScript tool (one typed `defineTool` function).
    New {
        /// Tool name in lowercase (e.g. weather, clickup-sync). Omit for interactive mode.
        name: Option<String>,
        /// Bundle namespace (free-form, no `/`). Defaults to `support`.
        #[arg(long)]
        bundle: Option<String>,
        /// Human-readable description.
        #[arg(long)]
        description: Option<String>,
        /// Output directory (default: ./<name>).
        #[arg(long)]
        output: Option<String>,
        /// Validate and print planned changes without writing (non-interactive only).
        #[arg(long)]
        dry_run: bool,
    },
    /// Derive the descriptor + digest for a tool project (writes tool-descriptor.json).
    Build {
        /// Tool project directory (default: current directory).
        #[arg(default_value = ".")]
        dir: String,
    },
    /// Run one invocation through the SDK over a stdio DevBackend.
    Test {
        /// Tool project directory (default: current directory).
        #[arg(default_value = ".")]
        dir: String,
        /// JSON input for the invocation (default: `{}`).
        #[arg(long)]
        input: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show resolved project config
    Show {
        #[arg(long)]
        config: Option<String>,
    },
    /// Set a config key (runner.url, project.default_agent, project.agent_path, runner.token_env)
    Set {
        key: String,
        value: String,
        #[arg(long)]
        config: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum InstallCommands {
    /// Publish agent source to repository and deploy to runner
    Agent {
        #[arg(long)]
        path: Option<String>,

        #[arg(long)]
        repository_url: Option<String>,

        #[arg(long)]
        url: Option<String>,

        #[arg(long, default_value = "published from source directory")]
        rationale: String,

        #[arg(long, value_enum, default_value_t = PublishOriginArg::Iteration)]
        origin: PublishOriginArg,

        #[arg(long)]
        runner_token: Option<String>,
    },
    /// Discover, approve, and import an external tool snapshot
    Tool {
        dir: String,
        #[arg(long)]
        repository_url: Option<String>,
        #[arg(long)]
        runner_token: Option<String>,
        #[arg(long)]
        sandbox_rootfs: Option<String>,
        #[arg(long)]
        approved_by: Option<String>,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum SkillCommands {
    /// Install a bundled Cursor skill (agent or tool authoring)
    Install {
        /// Skill kind: agent or tool
        kind: String,
        #[arg(long)]
        dest: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EvalCommands {
    /// Create sample eval/cases.toml
    Init {
        #[arg(long, default_value = "eval/cases.toml")]
        path: String,
    },
    /// Run eval cases against a deployed agent
    Run {
        #[arg(long, default_value = "eval/cases.toml")]
        manifest: String,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 0.0)]
        min_pass_rate: f64,
        #[arg(long, value_delimiter = ',')]
        cases: Option<Vec<String>>,
        #[arg(long)]
        deploy: bool,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        runner_token: Option<String>,
    },
    /// Print summary from the last eval run
    Report,
}

#[derive(Subcommand)]
pub enum ExternalToolCommands {
    /// Discover, approve, and import an external-tool snapshot into registry.
    Enable {
        /// Path to external tool directory (contains tool-manifest.json).
        dir: String,
        /// Repository URL to import the approved snapshot into.
        #[arg(long)]
        repository_url: Option<String>,
        /// Operator token for registry import. Falls back to RUNNER_TOKEN.
        #[arg(long)]
        runner_token: Option<String>,
        /// Bind sandbox rootfs path to use for runner-owned approval.
        #[arg(long)]
        sandbox_rootfs: Option<String>,
        /// Audit identity recorded as the approval owner (self-asserted).
        #[arg(long)]
        approved_by: Option<String>,
        /// Skip interactive approval prompt.
        #[arg(long)]
        yes: bool,
        /// Emit raw snapshot JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show approved and pending snapshots for a tool.
    Inspect {
        /// Tool name, e.g. support/weather.
        name: String,
        /// Legacy local snapshot cache root for inspect/offline workflows.
        #[arg(long)]
        cache_dir: Option<String>,
        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Re-discover an external tool and approve changed snapshot.
    Refresh {
        /// Tool name, e.g. support/weather.
        name: String,
        /// Path to external tool directory (contains tool-manifest.json).
        #[arg(long)]
        dir: String,
        /// Repository URL to import the approved snapshot into.
        #[arg(long)]
        repository_url: Option<String>,
        /// Operator token for registry import. Falls back to RUNNER_TOKEN.
        #[arg(long)]
        runner_token: Option<String>,
        /// Skip interactive approval prompt.
        #[arg(long)]
        yes: bool,
        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// List MCP servers in the repository registry.
    List {
        /// Repository base URL where repository routes are mounted.
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,
        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Discover, approve, and store an MCP server schema in the repository registry.
    Enable {
        /// Server id from mcp-servers.json.
        server_id: String,
        /// Path to mcp-servers.json.
        #[arg(long)]
        config: Option<String>,
        /// Repository base URL where repository routes are mounted.
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,
        /// Skip interactive approval prompt.
        #[arg(long)]
        yes: bool,
        /// Runner token for authenticated operator access (falls back to RUNNER_TOKEN env).
        #[arg(long)]
        runner_token: Option<String>,
    },
    /// Show latest or pinned MCP server snapshot summary.
    Server {
        /// Server id to inspect.
        server_id: String,
        /// Registry version to inspect. Defaults to latest.
        #[arg(long)]
        version: Option<u32>,
        /// Repository base URL where repository routes are mounted.
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,
        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// List registry versions for one MCP server.
    Versions {
        /// Server id to inspect.
        server_id: String,
        /// Repository base URL where repository routes are mounted.
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,
        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Lookup MCP registry entries by platform tool name.
    Tool {
        /// Platform tool name, e.g. mcp/meteo/get_meteo.
        platform_tool_name: String,
        /// Repository base URL where repository routes are mounted.
        #[arg(long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,
        /// Emit raw JSON.
        #[arg(long)]
        json: bool,
    },
}
