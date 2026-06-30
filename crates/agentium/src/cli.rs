// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Clap command definitions for `agentium`.

use clap::{Parser, Subcommand, ValueEnum};

use crate::{
    commands::publish::PublishOriginArg,
    templates::external_tool::{Access, InvocationMode, Language, Runtime, SandboxSource},
};

/// Agentium — unified platform host and developer SDK
#[derive(Parser)]
#[command(
    name = "agentium",
    version,
    about = "Agentium platform and developer SDK"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the Agentium platform (HTTP, stdio, repository, provenance)
    Serve {
        #[command(flatten)]
        runner: baml_agent_runner::RunnerCli,
    },

    /// Scaffold project config (`agentium.toml`) and optionally a first agent
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

    /// Inspect or update `agentium.toml`
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

    /// Create a standalone external tool scaffold (Rust/Bash/Python/TypeScript).
    /// This is the default for most users — it produces a self-contained
    /// directory the runner picks up via `BAML_EXTERNAL_TOOLS_DIR` without
    /// touching the platform workspace.
    NewTool {
        /// Tool name in lowercase (e.g., echo, clickup_sync, clickup-sync). Omit for interactive mode.
        name: Option<String>,

        /// Bundle namespace. Free-form string (e.g. `support`, `travel`, `acme`).
        /// Defaults to `support`. Must be non-empty and not contain `/`.
        #[arg(long)]
        bundle: Option<String>,

        /// Scaffold language
        #[arg(long, value_enum, default_value_t = Language::Rust)]
        lang: Language,

        /// Access level: read (default), write, or delete
        #[arg(long, value_enum)]
        access: Option<Access>,

        /// Runtime declaration to scaffold into tool-manifest.json
        #[arg(long, value_enum, default_value_t = Runtime::Process)]
        runtime: Runtime,

        /// Invocation contract to scaffold into tool-manifest.json.
        ///
        /// - `single-shot`: stateless `tool/invoke`
        /// - `session`: `tool/session_*` protocol (requires `--runtime sandbox`)
        #[arg(long, value_enum, default_value_t = InvocationMode::SingleShot)]
        invocation_mode: InvocationMode,

        /// Sandbox source kind when --runtime sandbox
        #[arg(long, value_enum, default_value_t = SandboxSource::Oci)]
        sandbox_source: SandboxSource,

        /// Sandbox OCI image reference (`...@sha256:...`) when --sandbox-source oci
        #[arg(long)]
        sandbox_image: Option<String>,

        /// Optional sandbox entrypoint argv, comma-separated
        #[arg(long, value_delimiter = ',')]
        sandbox_entrypoint: Vec<String>,

        /// For bind sandbox scaffolds, also generate Docker adapter artifacts
        /// and a Docker-assisted setup script (`setup_bind_sandbox.sh`).
        #[arg(long)]
        generate_docker: bool,

        /// Human-readable description for this tool
        #[arg(long)]
        description: Option<String>,

        /// Output directory (default: ./<name>)
        #[arg(long)]
        output: Option<String>,

        /// Validate and print planned changes without writing files; exits non-zero on validation failures (non-interactive only)
        #[arg(long)]
        dry_run: bool,
    },

    /// Create a new *static* tool crate — compiled into the platform workspace.
    /// Use this only when extending the platform itself (e.g., adding a
    /// system-level bundle). For every other case, prefer `new-tool`.
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

    /// Validate standalone external tool manifest against runtime parser
    CheckExternalTool {
        /// Path to external tool directory (contains tool-manifest.json)
        #[arg(long, default_value = ".")]
        path: String,
    },

    /// Sync bind sandbox metadata with a concrete rootfs directory.
    ///
    /// Optional Docker-assisted mode can build/export rootfs first.
    SandboxBindSync {
        /// Path to external tool directory (contains tool-manifest.json)
        #[arg(long)]
        tool_dir: String,

        /// Bind rootfs path. Relative paths resolve against --tool-dir.
        /// Defaults to runtime.image.path from tool-manifest.json.
        #[arg(long)]
        rootfs: Option<String>,

        /// Optional Dockerfile path for Docker-assisted build/export mode.
        /// Relative paths resolve against --tool-dir. When --image is provided
        /// and --dockerfile is omitted, defaults to adapter/Dockerfile.
        #[arg(long)]
        dockerfile: Option<String>,

        /// Optional Docker image tag/name for Docker-assisted build/export mode.
        /// Required to build/export from Docker; kept explicit to avoid using
        /// an unintended local image tag.
        #[arg(long)]
        image: Option<String>,

        /// Recreate rootfs directory when it already exists.
        #[arg(long)]
        force: bool,

        /// Run check-external-tool after patching metadata.
        #[arg(long)]
        check: bool,

        /// Validate and print planned values without writing metadata.
        #[arg(long)]
        dry_run: bool,

        /// Emit machine-readable JSON summary.
        #[arg(long)]
        json: bool,
    },

    /// Materialize sandbox sidecar bundle for OCI runtime metadata.
    ///
    /// Writes `tool-bundle.json` using shared sidecar helpers from metadata.
    SandboxOciPrepare {
        /// Path to external tool directory (contains tool-manifest.json)
        #[arg(long, default_value = ".")]
        tool_dir: String,

        /// Output path for generated tool-bundle.json.
        /// Relative paths resolve against --tool-dir.
        ///
        /// Default: adapter/sidecars/etc/agentium/tool-bundle.json
        #[arg(long)]
        output: Option<String>,

        /// Run check-external-tool after writing bundle.
        #[arg(long)]
        check: bool,

        /// Validate and print planned values without writing output.
        #[arg(long)]
        dry_run: bool,

        /// Emit machine-readable JSON summary.
        #[arg(long)]
        json: bool,
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

        /// Agent instance identifier
        #[arg(long, default_value = "default")]
        instance: String,

        /// Print debug diagnostics
        #[arg(long, short)]
        verbose: bool,
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
pub(crate) fn parse_access_or_default(raw: &str) -> Access {
    Access::from_str(raw.trim(), true).unwrap_or(Access::Read)
}

/// Parse an interactive-prompt string into a [`Language`]; fall back to Rust
/// when the user typed something unexpected.
pub(crate) fn parse_language_or_default(raw: &str) -> Language {
    Language::from_str(raw.trim(), true).unwrap_or(Language::Rust)
}

/// Parse an interactive-prompt string into a [`Runtime`]; fall back to Process
/// when the user typed something unexpected.
pub(crate) fn parse_runtime_or_default(raw: &str) -> Runtime {
    Runtime::from_str(raw.trim(), true).unwrap_or(Runtime::Process)
}
