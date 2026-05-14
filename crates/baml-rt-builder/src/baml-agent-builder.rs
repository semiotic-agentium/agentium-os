//! BAML Agent Builder
//!
//! This binary compiles, type-checks, and packages BAML + TypeScript agent applications
//! into distributable tar.gz packages, and runs agents with stdin/stdout connectivity.
//!
//! TypeScript compilation and type checking are delegated to `tsc`.

#![recursion_limit = "256"]

use std::{
    collections::HashSet,
    fs,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context as _, Result};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, BuilderService, FileSystem, FunctionName, PackagePath,
    RuntimeTypeGenerator, StdFileSystem, StdPackager, TscCompiler, TypeGenerator,
    bootstrap::{run_bootstrap, slug_from_name},
};
use baml_rt_core::ids::AgentId;
use baml_rt_observability::{spans, tracing_setup};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge};
use baml_rt_tools::{
    ManifestToolNames, ToolAccessPolicy, external_tools::build_builder_catalog,
    parse_access_allowlist, register_manifest_tools, tool_catalog::ToolCatalog,
};
use baml_rt_tools_claude as _;
use baml_tools_calculator as _;
#[cfg(feature = "clickup")]
use baml_tools_clickup as _;
#[cfg(feature = "notion")]
use baml_tools_notion as _;
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _;
#[cfg(feature = "slack")]
use baml_tools_slack as _;
use baml_tools_system as _;
use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "baml-agent-builder")]
#[command(about = "Build and run BAML agent packages", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Type-check TypeScript source code (generates types then runs tsc --noEmit)
    Lint {
        /// Agent directory (default: current directory)
        #[arg(short, long, default_value = ".")]
        agent_dir: PathBuf,
    },

    /// Package an agent into a tar.gz file (includes type checking)
    Package {
        /// Agent directory (default: current directory)
        #[arg(short, long, default_value = ".")]
        agent_dir: PathBuf,

        /// Output file path
        #[arg(short, long, default_value = "agent-package.tar.gz")]
        output: PathBuf,
    },

    /// Run an agent package with stdin/stdout connectivity
    Run {
        /// Agent package file path
        #[arg(short, long)]
        package: PathBuf,

        /// Function to call (if not provided, reads from stdin)
        #[arg(short, long)]
        function: Option<String>,

        /// JSON arguments (if not provided and function specified, reads from stdin)
        #[arg(short, long)]
        args: Option<String>,
    },

    /// Publish an agent source bundle to a repository and optionally deploy it.
    ///
    /// Sends source files to the repository; the repository builds and stores
    /// the deployable artifact under the canonical content hash.
    Publish {
        /// Agent directory (default: current directory)
        #[arg(short, long, default_value = ".")]
        agent_dir: PathBuf,

        /// Repository URL (e.g. http://127.0.0.1:18080/repository)
        #[arg(short, long, default_value = "http://127.0.0.1:18080/repository")]
        repository_url: String,

        /// Runner URL for deployment after publishing (e.g. http://127.0.0.1:18080).
        /// When set, automatically sends POST /deploy after a successful publish.
        #[arg(long)]
        deploy_url: Option<String>,

        /// Change rationale / commit message for this version.
        #[arg(short, long, default_value = "Published via baml-agent-builder")]
        message: String,

        /// Publish origin: `original` (new lineage) or `iteration` (next version).
        #[arg(long, default_value = "iteration")]
        origin: String,

        /// Runner token for authenticated operator access (falls back to RUNNER_TOKEN env)
        #[arg(long)]
        runner_token: Option<String>,
    },
    /// Re-discover an already-approved MCP server and flip its snapshot to
    /// `stale` if the server-config or per-tool input schemas have drifted.
    ///
    /// Pull-side companion to runtime push-drift: this is the operator's
    /// confirmation step. After review the operator re-runs `mcp-enable`
    /// to re-import and re-approve.
    McpReview {
        /// Server id to review (must match an entry in the cache).
        server_id: String,
        /// Path to mcp-servers.json (default: $HOME/.agent-platform/mcp-servers.json).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Snapshot cache root (default: $HOME/.agent-platform/mcp).
        #[arg(long)]
        cache_root: Option<PathBuf>,
        /// Show the diff without rewriting any cache entries.
        #[arg(long)]
        dry_run: bool,
    },
    /// Import and approve an MCP server's tool schemas into the local snapshot cache.
    McpEnable {
        /// Server id to enable (must match an entry under `mcpServers` in the config file).
        server_id: String,
        /// Path to mcp-servers.json (default: $HOME/.agent-platform/mcp-servers.json).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Snapshot cache root (default: $HOME/.agent-platform/mcp).
        #[arg(long)]
        cache_root: Option<PathBuf>,
        /// Skip the interactive approval prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Bootstrap a new BAML agent package (interactive TUI, or non-interactive with --name/--description)
    Bootstrap {
        /// Directory to create the package in (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Agent name (non-interactive: skip TUI when set with --description)
        #[arg(long)]
        name: Option<String>,
        /// Description (non-interactive: skip TUI when set with --name)
        #[arg(long)]
        description: Option<String>,
        /// Include no tools (non-interactive; only used when --name and --description are set)
        #[arg(long)]
        no_tools: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_setup::init_tracing();
    match dotenvy::dotenv() {
        Ok(path) => tracing::debug!(path = ?path, "Loaded .env"),
        Err(err) => tracing::debug!(error = ?err, "No .env loaded"),
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Lint { agent_dir } => {
            let agent_dir = AgentDir::new(agent_dir)?;
            lint_agent(&agent_dir).await?;
        }
        Commands::Package { agent_dir, output } => {
            let agent_dir = AgentDir::new(agent_dir)?;
            package_agent(&agent_dir, &output).await?;
        }
        Commands::Run {
            package,
            function,
            args,
        } => {
            let package_path = PackagePath::new(package)?;
            let function_name = function.map(FunctionName::new).transpose()?;
            run_agent(&package_path, function_name.as_ref(), args.as_deref()).await?;
        }
        Commands::Publish {
            agent_dir,
            repository_url,
            deploy_url,
            message,
            origin,
            runner_token,
        } => {
            let agent_dir = AgentDir::new(agent_dir)?;
            publish_agent(
                &agent_dir,
                &repository_url,
                deploy_url.as_deref(),
                &message,
                &origin,
                runner_token.as_deref(),
            )
            .await?;
        }
        Commands::McpEnable {
            server_id,
            config,
            cache_root,
            yes,
        } => {
            mcp_enable(&server_id, config.as_deref(), cache_root.as_deref(), yes).await?;
        }
        Commands::McpReview {
            server_id,
            config,
            cache_root,
            dry_run,
        } => {
            mcp_review(
                &server_id,
                config.as_deref(),
                cache_root.as_deref(),
                dry_run,
            )
            .await?;
        }
        Commands::Bootstrap {
            path,
            name,
            description,
            no_tools,
        } => {
            bootstrap_agent(&path, name.as_deref(), description.as_deref(), no_tools).await?;
        }
    }

    Ok(())
}

async fn bootstrap_agent(
    path: &PathBuf,
    name_arg: Option<&str>,
    description_arg: Option<&str>,
    _no_tools: bool,
) -> Result<()> {
    let is_current_dir = path.as_os_str() == "." || path == &PathBuf::from(".");
    let resolved = path.canonicalize().unwrap_or_else(|_| path.clone());
    let default_name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-agent")
        .to_string();

    let (name, description, tool_ids): (String, String, Vec<String>) = if let (Some(n), Some(d)) =
        (name_arg, description_arg)
    {
        let tool_ids = vec![];
        (n.to_string(), d.to_string(), tool_ids)
    } else {
        let name = inquire::Text::new("Agent name")
            .with_default(&default_name)
            .with_help_message("Display name for the agent (used for slug in manifest)")
            .prompt()
            .context("Prompt cancelled or failed")?;

        let description = inquire::Text::new("Description")
            .with_help_message("Short description of the agent")
            .prompt()
            .context("Prompt cancelled or failed")?;

        let catalog = build_builder_catalog()?;
        let tool_options: Vec<(String, String)> = catalog
            .iter()
            .map(|m| {
                let id = m.name.to_string();
                let label = format!("{} — {}", id, m.description);
                (label, id)
            })
            .collect();
        let tool_ids: Vec<String> = if tool_options.is_empty() {
            Vec::new()
        } else {
            let choices: Vec<String> = tool_options
                .iter()
                .map(|(label, _)| label.clone())
                .collect();
            let raw = inquire::MultiSelect::new("Tools (space to select, type to filter)", choices)
                .with_help_message("Select tools to include in the agent. You can type to search.")
                .prompt()
                .context("Prompt cancelled or failed")?;
            raw.into_iter()
                .filter_map(|label| {
                    tool_options
                        .iter()
                        .find(|(l, _)| *l == label)
                        .map(|(_, id)| id.clone())
                })
                .collect()
        };
        (name, description, tool_ids)
    };

    let out_path = if is_current_dir {
        std::env::current_dir()
            .context("Failed to resolve current directory")?
            .join(slug_from_name(name.trim()))
    } else {
        resolved
    };

    println!("Creating package at {}...", out_path.display());
    run_bootstrap(&out_path, name.trim(), description.trim(), &tool_ids).await?;
    println!("✅ Bootstrap complete: {}", out_path.display());
    println!(
        "   Next: cd {} && baml-agent-builder lint",
        out_path.display()
    );
    Ok(())
}

async fn lint_agent(agent_dir: &AgentDir) -> Result<()> {
    let span = spans::lint_agent(agent_dir.as_path());
    let _guard = span.enter();

    println!("🔍 Type-checking agent...");

    // Generate runtime types first so tsc can resolve them
    let build_dir = BuildDir::new()?;
    let filesystem = StdFileSystem;
    filesystem.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;

    let type_generator = RuntimeTypeGenerator::new();
    type_generator.generate(agent_dir, &build_dir).await?;

    // Run tsc --noEmit (type check only)
    let ts_compiler = TscCompiler::new();
    ts_compiler.typecheck(agent_dir).await?;

    println!("✓ Type checking passed");
    Ok(())
}

async fn package_agent(agent_dir: &AgentDir, output: &std::path::Path) -> Result<()> {
    let span = spans::package_agent(agent_dir.as_path(), output);
    let _guard = span.enter();

    println!("📦 Building agent package...");
    println!("   Agent directory: {}", agent_dir);
    println!("   Output: {}", output.display());

    // Create temporary build directory
    let build_dir = BuildDir::new()?;

    // Initialize services
    let filesystem = StdFileSystem;
    let ts_compiler = TscCompiler::new();
    let type_generator = RuntimeTypeGenerator::new();
    let packager = StdPackager::new(filesystem);

    // Copy baml_src to build directory (runtime loads from baml_src)
    filesystem.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;

    let builder_service = BuilderService::new(ts_compiler, type_generator, packager);

    // Build the package
    builder_service
        .build_package(agent_dir, &build_dir, output)
        .await?;

    println!(
        "\n✅ Agent package built successfully: {}",
        output.display()
    );
    Ok(())
}

/// Pure token resolution from explicit sources (testable without env mutation).
fn resolve_builder_token_from_sources(
    flag: Option<&str>,
    env_value: Option<String>,
) -> Result<Option<String>> {
    let trimmed = match flag {
        Some(v) => Some(v.trim().to_owned()),
        None => env_value.map(|v| v.trim().to_owned()),
    };
    match trimmed {
        Some(v) if v.is_empty() => {
            anyhow::bail!(
                "Runner token is empty or whitespace-only. \
                 Provide a valid token via --runner-token or RUNNER_TOKEN."
            );
        }
        Some(v) => Ok(Some(v)),
        None => Ok(None),
    }
}

/// Resolve runner token from CLI flag with `RUNNER_TOKEN` env fallback.
fn resolve_builder_token(flag: Option<&str>) -> Result<Option<String>> {
    resolve_builder_token_from_sources(flag, std::env::var("RUNNER_TOKEN").ok())
}

fn check_response(
    status: reqwest::StatusCode,
    body: &str,
    op_name: &str,
    authenticated: bool,
) -> Result<()> {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let hint = if authenticated {
            "Hint: the runner token was rejected \
             — verify it matches the server's RUNNER_TOKEN."
        } else {
            "Hint: pass --runner-token <token> or set the \
             RUNNER_TOKEN environment variable."
        };
        anyhow::bail!("{op_name} failed ({status}): {body}. {hint}");
    }
    if !status.is_success() {
        anyhow::bail!("{op_name} failed ({status}): {body}");
    }
    Ok(())
}

async fn publish_agent(
    agent_dir: &AgentDir,
    repository_url: &str,
    deploy_url: Option<&str>,
    message: &str,
    origin_str: &str,
    runner_token_flag: Option<&str>,
) -> Result<()> {
    use baml_rt_repository::{
        commands::{PublishCommand, PublishOrigin, PublishResult},
        entry::ChangeRationale,
        source_bundle_from_agent_dir,
    };

    let token = resolve_builder_token(runner_token_flag)?;

    println!("📦 Publishing source bundle...");

    let repo_url = repository_url.trim_end_matches('/');
    let http = reqwest::Client::new();

    let (name, source) = source_bundle_from_agent_dir(agent_dir.as_path())
        .map_err(|e| anyhow::anyhow!("Failed to read source bundle from agent directory: {e}"))?;

    let origin = match origin_str {
        "original" => PublishOrigin::Original,
        _ => PublishOrigin::Iteration,
    };
    let rationale = ChangeRationale::new(message)
        .map_err(|_| anyhow::anyhow!("Change rationale must not be empty"))?;

    let cmd = PublishCommand {
        name: name.clone(),
        source,
        rationale,
        origin,
    };

    println!("   Publishing entry...");
    let mut pub_request = http.post(format!("{repo_url}/publish")).json(&cmd);
    if let Some(ref t) = token {
        pub_request = pub_request.header("X-Runner-Token", t.as_str());
    }
    let pub_resp = pub_request
        .send()
        .await
        .context("Failed to publish entry")?;

    let status = pub_resp.status();
    let body = pub_resp.text().await.unwrap_or_default();
    check_response(status, &body, "Publish", token.is_some())?;

    let result: PublishResult =
        serde_json::from_str(&body).context("Failed to parse publish response")?;

    let content_hash = result.hash.as_str();
    println!(
        "\n✅ Published {name}@v{version}",
        name = result.version_ref.name,
        version = result.version_ref.version,
    );
    println!("   content_hash: {content_hash}");

    // Optionally deploy immediately using content_hash (not blob_hash).
    if let Some(runner_url) = deploy_url {
        println!("\n🚀 Deploying {content_hash} to {runner_url}...");
        let deploy_body = serde_json::json!({ "hash": content_hash });
        let mut deploy_request = http
            .post(format!("{}/deploy", runner_url.trim_end_matches('/')))
            .json(&deploy_body);
        if let Some(ref t) = token {
            deploy_request = deploy_request.header("X-Runner-Token", t.as_str());
        }
        let deploy_resp = deploy_request
            .send()
            .await
            .context("Failed to send deploy request")?;
        let deploy_status = deploy_resp.status();
        let body = deploy_resp.text().await.unwrap_or_default();
        check_response(deploy_status, &body, "Deploy", token.is_some())?;
        let body: serde_json::Value =
            serde_json::from_str(&body).context("Failed to parse deploy response")?;
        println!("✅ Deployed: {body}");
    }

    Ok(())
}
async fn run_agent(
    package_path: &PackagePath,
    function: Option<&FunctionName>,
    args_json: Option<&str>,
) -> Result<()> {
    let span = spans::load_agent_package(package_path.as_path());
    let _guard = span.enter();

    // Load the agent package
    println!("📦 Loading agent package: {}", package_path);
    let access_policy = parse_access_allowlist();
    let agent = load_agent_package(package_path.as_path(), &access_policy).await?;
    println!("✅ Agent loaded: {}", agent.name());

    // If function is specified, call it once
    if let Some(function_name) = function {
        let args = if let Some(args_str) = args_json {
            serde_json::from_str(args_str).context("Invalid JSON args")?
        } else {
            // Read args from stdin
            let mut input = String::new();
            io::stdin()
                .read_line(&mut input)
                .context("Failed to read from stdin")?;
            serde_json::from_str(input.trim()).context("Invalid JSON from stdin")?
        };

        let invoke_span = spans::invoke_function(None, "agent", function_name.as_str());
        let _invoke_guard = invoke_span.enter();
        let result = agent.invoke_function(function_name.as_str(), args).await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // Otherwise, run in interactive mode: read from stdin, write to stdout
    println!("🔄 Running in interactive mode (reading from stdin, writing to stdout)");
    println!("   Format: <function_name> <json_args>");
    println!(
        "   Example: onChatMessage receives {{ parts: [{{\"text\":\"Alice\"}}] }} (IDs/role are host-managed)"
    );
    println!("   Press Ctrl+D to exit\n");

    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        print!("> ");
        io::stdout().flush().context("Failed to flush stdout")?;

        match stdin_lock.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Parse input: function_name json_args
                let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
                if parts.len() < 2 {
                    eprintln!("Error: Expected format: <function_name> <json_args>");
                    continue;
                }

                let function_name_str = parts[0];
                let args_json = parts[1];

                match serde_json::from_str::<Value>(args_json) {
                    Ok(args) => {
                        let invoke_span = spans::invoke_function(None, "agent", function_name_str);
                        let _invoke_guard = invoke_span.enter();
                        match agent.invoke_function(function_name_str, args).await {
                            Ok(result) => {
                                println!("{}", serde_json::to_string_pretty(&result)?);
                            }
                            Err(e) => {
                                eprintln!("Error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error: Invalid JSON: {}", e);
                    }
                }
            }
            Err(e) => {
                return Err(e).context("Failed to read from stdin");
            }
        }
    }

    println!("\n👋 Exiting");
    Ok(())
}

// Agent package loader (reusing logic from baml-agent-runner)
struct LoadedAgent {
    name: String,
    agent_id: baml_rt_core::ids::AgentId,
    js_bridge: Arc<Mutex<QuickJSBridge>>,
}

impl LoadedAgent {
    fn name(&self) -> &str {
        &self.name
    }

    async fn invoke_function(&self, function_name: &str, args: Value) -> Result<Value> {
        let scope =
            baml_rt_core::context::InvocationScope::synthetic_message(self.agent_id.clone());
        Ok(QuickJSBridge::invoke_js_function_nonblocking(
            self.js_bridge.clone(),
            &scope,
            function_name,
            args,
        )
        .await?)
    }
}

async fn load_agent_package(
    package_path: &std::path::Path,
    policy: &ToolAccessPolicy,
) -> Result<LoadedAgent> {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    // Extract package
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("System clock is before UNIX epoch")?;

    let extract_dir = std::env::temp_dir().join(format!("baml-agent-{}", timestamp.as_secs()));
    fs::create_dir_all(&extract_dir).context("Failed to create extraction directory")?;

    let tar_gz = fs::File::open(package_path).context("Failed to open package file")?;
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    archive
        .unpack(&extract_dir)
        .context("Failed to unpack archive")?;

    // Load manifest
    let manifest_path = extract_dir.join("manifest.json");
    let manifest_content =
        fs::read_to_string(&manifest_path).context("Failed to read manifest.json")?;
    let manifest_json: Value =
        serde_json::from_str(&manifest_content).context("Failed to parse manifest.json")?;

    let name = manifest_json
        .get("name")
        .and_then(|v| v.as_str())
        .context("manifest.json missing 'name' field")?
        .to_string();

    let entry_point = manifest_json
        .get("entry_point")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "dist/index.js".to_string());

    // Load BAML schema
    let baml_src = extract_dir.join("baml_src");
    let runtime_manager = {
        let schema_span = spans::load_baml_schema(&baml_src);
        let _schema_guard = schema_span.enter();
        let baml_src_str = baml_src.to_str().with_context(|| {
            format!(
                "BAML source path contains invalid UTF-8: {}",
                baml_src.display()
            )
        })?;
        let mut rm = BamlRuntimeManager::builder()
            .with_default_fnox_llm_resolver()
            .build()?;
        rm.load_schema(baml_src_str)?;

        let tools = manifest_json
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if !tools.is_empty() {
            rm.set_tool_allowlist(tools.iter().cloned().collect::<HashSet<_>>())
                .await?;
            let manifest_tool_names = ManifestToolNames::parse(&tools)?;
            register_manifest_tools(rm.tool_registry().as_ref(), &manifest_tool_names, policy)?;
        }

        rm.rebuild_function_tool_manifest();
        rm
    };

    // Create QuickJS bridge
    let runtime_manager_arc = Arc::new(tokio::sync::RwLock::new(runtime_manager));
    // Generate a temporary agent_id for builder context
    let temp_agent_id = AgentId::from_uuid(baml_rt_core::ids::UuidId::new(Uuid::new_v4()));
    let mut js_bridge = {
        let bridge_span = spans::create_js_bridge();
        let _bridge_guard = bridge_span.enter();
        let mut bridge =
            QuickJSBridge::new(runtime_manager_arc.clone(), temp_agent_id.clone()).await?;
        bridge.register_baml_functions().await?;
        bridge
    };

    // Load agent JavaScript code
    let entry_point_path = extract_dir.join(&entry_point);
    if entry_point_path.exists() {
        let eval_span = spans::evaluate_agent_code(&entry_point);
        let _eval_guard = eval_span.enter();
        let agent_code = fs::read_to_string(&entry_point_path).with_context(|| {
            format!("Failed to read entry point {}", entry_point_path.display())
        })?;
        // Execute agent code - this should set up functions on globalThis
        if let Err(e) = js_bridge.eval_sync(&agent_code).await {
            tracing::warn!(error = ?e, "Agent init script evaluation failed");
        }
    } else {
        tracing::warn!(entry_point = %entry_point_path.display(), "Agent entry point not found");
    }

    Ok(LoadedAgent {
        name,
        agent_id: temp_agent_id,
        js_bridge: Arc::new(Mutex::new(js_bridge)),
    })
}

async fn mcp_enable(
    server_id: &str,
    config_path: Option<&std::path::Path>,
    cache_root: Option<&std::path::Path>,
    skip_prompt: bool,
) -> Result<()> {
    use anyhow::bail;
    use baml_rt_tools::{
        mcp_cache::{default_cache_root, write_snapshot},
        mcp_config::McpServersFile,
        mcp_snapshot::McpApprovalState,
    };
    use baml_tools_mcp::importer::{EnvSecretResolver, ImportOptions, Importer};

    let config_path: PathBuf = match config_path {
        Some(p) => p.to_path_buf(),
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --config explicitly"))?;
            home.join(".agent-platform").join("mcp-servers.json")
        }
    };
    let cache_root: PathBuf = match cache_root {
        Some(p) => p.to_path_buf(),
        None => default_cache_root()
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --cache-root explicitly"))?,
    };

    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading mcp-servers config at {}", config_path.display()))?;
    let parsed = McpServersFile::parse(&raw)
        .with_context(|| format!("parsing {}", config_path.display()))?;
    let Some(server_config) = parsed.servers.get(server_id) else {
        bail!(
            "server `{server_id}` not found in {}; available: {:?}",
            config_path.display(),
            parsed.servers.keys().collect::<Vec<_>>()
        );
    };

    println!(
        "Importing MCP server `{server_id}` from {}",
        config_path.display()
    );
    let importer = Importer::new(&EnvSecretResolver);
    let mut snapshot = importer
        .import(
            server_config,
            ImportOptions {
                server_id: server_id.to_string(),
                sandbox_profile: None,
            },
        )
        .await
        .with_context(|| format!("importing MCP server `{server_id}`"))?;

    println!();
    println!(
        "Server: {server_id}\n  protocol_version: {}\n  server_config_digest: {}",
        snapshot.protocol_version, snapshot.server_config_digest,
    );
    if let Some(info) = &snapshot.server_info {
        println!("  server_info: {}", info);
    }
    if !snapshot.secret_refs.is_empty() {
        println!(
            "  secret_refs: {}",
            snapshot
                .secret_refs
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("\nTools ({}):", snapshot.tools.len());
    for tool in &snapshot.tools {
        println!(
            "  - {}\n      mcp_name: {}\n      access: {}\n      schema_digest: {}\n      output_mode: {:?}{}",
            tool.platform_tool_name,
            tool.mcp_tool_name,
            tool.access_level,
            tool.input_schema_digest,
            tool.output_mode,
            tool.opaque_fallback_reason
                .as_deref()
                .map(|r| format!("\n      opaque_fallback: {r}"))
                .unwrap_or_default(),
        );
    }
    println!();

    let approve = if skip_prompt {
        true
    } else {
        inquire::Confirm::new("Approve this server and all tools?")
            .with_default(false)
            .prompt()
            .unwrap_or(false)
    };

    if !approve {
        println!("Aborted. No snapshot written.");
        return Ok(());
    }

    let owner = std::env::var("MCP_APPROVER_EMAIL")
        .ok()
        .or_else(|| std::env::var("GIT_AUTHOR_EMAIL").ok());
    let reviewed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("epoch:{}", d.as_secs()))
        .ok();
    let prior_state = snapshot.approval.state;
    snapshot.approval.state = McpApprovalState::Approved;
    tracing::info!(
        target: "mcp.approval",
        mcp_server_id = %server_id,
        event = "mcp.approval_transition",
        from = ?prior_state,
        to = ?McpApprovalState::Approved,
        owner = ?owner,
        "MCP server approved via mcp-enable",
    );
    snapshot.approval.owner = owner.clone();
    snapshot.approval.reviewed_at = reviewed_at.clone();
    for tool in &mut snapshot.tools {
        tool.approval.state = McpApprovalState::Approved;
        tool.approval.owner = owner.clone();
        tool.approval.reviewed_at = reviewed_at.clone();
    }

    fs::create_dir_all(&cache_root)
        .with_context(|| format!("creating cache root at {}", cache_root.display()))?;
    write_snapshot(&cache_root, &snapshot)
        .with_context(|| format!("writing snapshot under {}", cache_root.display()))?;
    println!(
        "Approved and wrote {} tool(s) under {}",
        snapshot.tools.len(),
        cache_root.display()
    );
    Ok(())
}

/// Pull-drift detection. Re-runs the importer in the same Tier 1 sandbox,
/// compares `server_config_digest` and per-tool `input_schema_digest` against
/// the cached snapshot, and flips records to `Stale` when anything drifted.
async fn mcp_review(
    server_id: &str,
    config_path: Option<&std::path::Path>,
    cache_root: Option<&std::path::Path>,
    dry_run: bool,
) -> Result<()> {
    use anyhow::bail;
    use baml_rt_tools::{
        mcp_cache::{default_cache_root, mark_server_stale, mark_tool_stale, read_snapshot},
        mcp_config::McpServersFile,
        mcp_snapshot::McpApprovalState,
    };
    use baml_tools_mcp::importer::{EnvSecretResolver, ImportOptions, Importer};

    let config_path: PathBuf = match config_path {
        Some(p) => p.to_path_buf(),
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --config explicitly"))?;
            home.join(".agent-platform").join("mcp-servers.json")
        }
    };
    let cache_root: PathBuf = match cache_root {
        Some(p) => p.to_path_buf(),
        None => default_cache_root()
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --cache-root explicitly"))?,
    };

    let cached = read_snapshot(&cache_root, server_id)
        .with_context(|| format!("reading cached snapshot for `{server_id}`"))?;

    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading mcp-servers config at {}", config_path.display()))?;
    let parsed = McpServersFile::parse(&raw)
        .with_context(|| format!("parsing {}", config_path.display()))?;
    let Some(server_config) = parsed.servers.get(server_id) else {
        bail!(
            "server `{server_id}` not in {}; available: {:?}",
            config_path.display(),
            parsed.servers.keys().collect::<Vec<_>>()
        );
    };

    println!("Re-discovering MCP server `{server_id}` for drift review...");
    let importer = Importer::new(&EnvSecretResolver);
    let fresh = importer
        .import(
            server_config,
            ImportOptions {
                server_id: server_id.to_string(),
                sandbox_profile: None,
            },
        )
        .await
        .with_context(|| format!("re-importing MCP server `{server_id}`"))?;

    let mut drifted_tools: Vec<(String, String, String)> = Vec::new();
    let server_drifted = cached.server_config_digest != fresh.server_config_digest;

    let cached_by_name: std::collections::BTreeMap<&str, &_> = cached
        .tools
        .iter()
        .map(|t| (t.platform_tool_name.as_str(), t))
        .collect();
    let fresh_by_name: std::collections::BTreeMap<&str, &_> = fresh
        .tools
        .iter()
        .map(|t| (t.platform_tool_name.as_str(), t))
        .collect();

    for (name, fresh_tool) in &fresh_by_name {
        match cached_by_name.get(name) {
            Some(cached_tool) => {
                if cached_tool.input_schema_digest != fresh_tool.input_schema_digest {
                    drifted_tools.push((
                        (*name).to_string(),
                        cached_tool.input_schema_digest.to_string(),
                        fresh_tool.input_schema_digest.to_string(),
                    ));
                }
            }
            None => {
                drifted_tools.push((
                    (*name).to_string(),
                    "<absent>".into(),
                    fresh_tool.input_schema_digest.to_string(),
                ));
            }
        }
    }
    for (name, cached_tool) in &cached_by_name {
        if !fresh_by_name.contains_key(name) {
            drifted_tools.push((
                (*name).to_string(),
                cached_tool.input_schema_digest.to_string(),
                "<absent>".into(),
            ));
        }
    }

    if !server_drifted && drifted_tools.is_empty() {
        println!("No drift detected for `{server_id}`. Snapshot is up to date.");
        return Ok(());
    }

    println!("\nDrift detected for `{server_id}`:");
    if server_drifted {
        println!(
            "  server_config_digest: {} -> {}",
            cached.server_config_digest, fresh.server_config_digest
        );
    }
    for (name, before, after) in &drifted_tools {
        println!("  tool {name}: {before} -> {after}");
    }

    if dry_run {
        println!("\nDry run — no cache entries modified.");
        return Ok(());
    }

    if server_drifted || !drifted_tools.is_empty() {
        let prev = mark_server_stale(&cache_root, server_id)
            .with_context(|| format!("marking server `{server_id}` stale"))?;
        if prev != McpApprovalState::Stale {
            tracing::warn!(
                target: "mcp.drift",
                mcp_server_id = %server_id,
                event = "mcp.approval_transition",
                from = ?prev,
                to = ?McpApprovalState::Stale,
                "MCP server transitioned to stale via mcp-review",
            );
        }
    }
    for (name, _, _) in &drifted_tools {
        // Only mark tools that still exist in the cache (the "<absent>" case
        // for newly-added tools has no corresponding tool record yet).
        if cached_by_name.contains_key(name.as_str()) {
            let prev = mark_tool_stale(&cache_root, name)
                .with_context(|| format!("marking tool `{name}` stale"))?;
            if prev != McpApprovalState::Stale {
                tracing::warn!(
                    target: "mcp.drift",
                    mcp_server_id = %server_id,
                    mcp_tool = %name,
                    event = "mcp.approval_transition",
                    from = ?prev,
                    to = ?McpApprovalState::Stale,
                    "MCP tool transitioned to stale via mcp-review",
                );
            }
        }
    }

    println!(
        "\nMarked `{server_id}` as stale. Run `mcp-enable {server_id}` after vetting the new schemas to re-approve."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_builder_token_precedence() {
        // Flag takes precedence over env
        let result =
            resolve_builder_token_from_sources(Some("flag"), Some("env".to_string())).unwrap();
        assert_eq!(result.as_deref(), Some("flag"));

        // Env fallback
        let result = resolve_builder_token_from_sources(None, Some("env-val".to_string())).unwrap();
        assert_eq!(result.as_deref(), Some("env-val"));

        // Neither → None
        let result = resolve_builder_token_from_sources(None, None).unwrap();
        assert!(result.is_none());

        // Empty flag rejected
        let err = resolve_builder_token_from_sources(Some(""), None).unwrap_err();
        assert!(err.to_string().contains("empty or whitespace-only"));

        // Whitespace flag rejected
        let err = resolve_builder_token_from_sources(Some("   "), None).unwrap_err();
        assert!(err.to_string().contains("empty or whitespace-only"));

        // Empty env rejected
        let err = resolve_builder_token_from_sources(None, Some("".to_string())).unwrap_err();
        assert!(err.to_string().contains("empty or whitespace-only"));

        // Whitespace-padded token is trimmed
        let result = resolve_builder_token_from_sources(Some("  abc  "), None).unwrap();
        assert_eq!(result.as_deref(), Some("abc"));
    }
}
