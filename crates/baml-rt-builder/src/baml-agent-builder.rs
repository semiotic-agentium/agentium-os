//! BAML Agent Builder
//!
//! This binary compiles, lints, and packages BAML + TypeScript agent applications
//! into distributable tar.gz packages, and runs agents with stdin/stdout connectivity.
//!
//! Uses OXC for high-performance TypeScript compilation and linting.

#![recursion_limit = "256"]

use std::{
    collections::HashSet,
    fs,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

use baml_rt_builder::builder::{
    AgentDir, BuildDir, BuilderService, FileSystem, FunctionName, Linter, OxcLinter,
    OxcTypeScriptCompiler, PackagePath, RuntimeTypeGenerator, StdFileSystem, StdPackager,
    bootstrap::{run_bootstrap, slug_from_name},
};
use baml_rt_core::{BamlRtError, Result, ids::AgentId};
use baml_rt_observability::{spans, tracing_setup};
use baml_rt_quickjs::{BamlRuntimeManager, QuickJSBridge};
use baml_rt_tools::{
    enforce_tool_access, parse_access_allowlist, tool_catalog::all_tool_metadata, tools::ToolAccess,
};
use baml_rt_tools_system as _;
use clap::{Parser, Subcommand};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "baml-agent-builder")]
#[command(about = "Build and run BAML agent packages with OXC", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Lint TypeScript/JavaScript source code
    Lint {
        /// Agent directory (default: current directory)
        #[arg(short, long, default_value = ".")]
        agent_dir: PathBuf,
    },

    /// Package an agent into a tar.gz file
    Package {
        /// Agent directory (default: current directory)
        #[arg(short, long, default_value = ".")]
        agent_dir: PathBuf,

        /// Output file path
        #[arg(short, long, default_value = "agent-package.tar.gz")]
        output: PathBuf,

        /// Skip linting
        #[arg(long)]
        skip_lint: bool,
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
        Commands::Package {
            agent_dir,
            output,
            skip_lint,
        } => {
            let agent_dir = AgentDir::new(agent_dir)?;
            package_agent(&agent_dir, &output, !skip_lint).await?;
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
            .map_err(|e| {
                BamlRtError::InvalidArgument(format!("Prompt cancelled or failed: {}", e))
            })?;

        let description = inquire::Text::new("Description")
            .with_help_message("Short description of the agent")
            .prompt()
            .map_err(|e| {
                BamlRtError::InvalidArgument(format!("Prompt cancelled or failed: {}", e))
            })?;

        let tool_options: Vec<(String, String)> = all_tool_metadata()
            .into_iter()
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
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!("Prompt cancelled or failed: {}", e))
                })?;
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
            .map_err(BamlRtError::Io)?
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

    let filesystem = StdFileSystem;
    let linter = OxcLinter::new(filesystem);
    linter.lint(agent_dir).await
}

async fn package_agent(agent_dir: &AgentDir, output: &std::path::Path, lint: bool) -> Result<()> {
    let span = spans::package_agent(agent_dir.as_path(), output);
    let _guard = span.enter();

    println!("📦 Building agent package...");
    println!("   Agent directory: {}", agent_dir);
    println!("   Output: {}", output.display());

    // Create temporary build directory
    let build_dir = BuildDir::new()?;

    // Initialize services
    let filesystem = StdFileSystem;
    let linter = OxcLinter::new(filesystem);
    let ts_compiler = OxcTypeScriptCompiler::new(filesystem);
    let type_generator = RuntimeTypeGenerator::new();
    let packager = StdPackager::new(filesystem);

    // Copy baml_src to build directory (runtime loads from baml_src)
    filesystem.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;

    let builder_service = BuilderService::new(linter, ts_compiler, type_generator, packager);

    // Build the package
    builder_service
        .build_package(agent_dir, &build_dir, output, lint)
        .await?;

    println!(
        "\n✅ Agent package built successfully: {}",
        output.display()
    );
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
    let access_allowlist = parse_access_allowlist();
    let agent = load_agent_package(package_path.as_path(), &access_allowlist).await?;
    println!("✅ Agent loaded: {}", agent.name());

    // If function is specified, call it once
    if let Some(function_name) = function {
        let args = if let Some(args_str) = args_json {
            serde_json::from_str(args_str).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "Invalid JSON args".to_string(),
                source: Box::new(e),
            })?
        } else {
            // Read args from stdin
            let mut input = String::new();
            io::stdin().read_line(&mut input).map_err(BamlRtError::Io)?;
            serde_json::from_str(input.trim()).map_err(|e| {
                BamlRtError::InvalidArgumentWithSource {
                    message: "Invalid JSON from stdin".to_string(),
                    source: Box::new(e),
                }
            })?
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
        io::stdout().flush().map_err(BamlRtError::Io)?;

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
                return Err(BamlRtError::Io(e));
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
        let mut bridge_guard = self.js_bridge.lock().await;
        bridge_guard
            .invoke_js_function(&scope, function_name, args)
            .await
    }
}

async fn load_agent_package(
    package_path: &std::path::Path,
    access_allowlist: &Option<HashSet<ToolAccess>>,
) -> Result<LoadedAgent> {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    // Extract package
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(BamlRtError::SystemTime)?;

    let extract_dir = std::env::temp_dir().join(format!("baml-agent-{}", timestamp.as_secs()));
    fs::create_dir_all(&extract_dir).map_err(BamlRtError::Io)?;

    let tar_gz = fs::File::open(package_path).map_err(BamlRtError::Io)?;
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    archive.unpack(&extract_dir).map_err(BamlRtError::Io)?;

    // Load manifest
    let manifest_path = extract_dir.join("manifest.json");
    let manifest_content = fs::read_to_string(&manifest_path).map_err(BamlRtError::Io)?;
    let manifest_json: Value =
        serde_json::from_str(&manifest_content).map_err(BamlRtError::Json)?;

    let name = manifest_json
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("manifest.json missing 'name' field".to_string())
        })?
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
        let baml_src_str = baml_src.to_str().ok_or_else(|| {
            BamlRtError::InvalidArgument(format!(
                "BAML source path contains invalid UTF-8: {}",
                baml_src.display()
            ))
        })?;
        let mut rm = BamlRuntimeManager::new()?;
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

            for tool_name in &tools {
                enforce_tool_access(tool_name, access_allowlist)?;
                match tool_name.as_str() {
                    "support/calculate" => {
                        rm.register_tool(baml_rt_tools::support::CalculatorTool)
                            .await?;
                    }
                    "support/notionSearchPages" => {
                        #[cfg(feature = "notion")]
                        {
                            rm.register_tool(baml_rt_tools::notion::NotionSearchPagesTool::new())
                                .await?;
                        }
                        #[cfg(not(feature = "notion"))]
                        {
                            return Err(BamlRtError::InvalidArgument(
                                "Notion tool not compiled: enable baml-rt-builder feature 'notion'"
                                    .to_string(),
                            ));
                        }
                    }
                    "support/notionGetPage" => {
                        #[cfg(feature = "notion")]
                        {
                            rm.register_tool(baml_rt_tools::notion::NotionGetPageTool::new())
                                .await?;
                        }
                        #[cfg(not(feature = "notion"))]
                        {
                            return Err(BamlRtError::InvalidArgument(
                                "Notion tool not compiled: enable baml-rt-builder feature 'notion'"
                                    .to_string(),
                            ));
                        }
                    }
                    "support/notionGetPageBlocks" => {
                        #[cfg(feature = "notion")]
                        {
                            rm.register_tool(baml_rt_tools::notion::NotionGetPageBlocksTool::new())
                                .await?;
                        }
                        #[cfg(not(feature = "notion"))]
                        {
                            return Err(BamlRtError::InvalidArgument(
                                "Notion tool not compiled: enable baml-rt-builder feature 'notion'"
                                    .to_string(),
                            ));
                        }
                    }
                    other => {
                        warn!(
                            tool = other,
                            "Unknown tool in manifest, skipping registration"
                        );
                    }
                }
            }
        }

        rm
    };

    // Create QuickJS bridge
    let runtime_manager_arc = Arc::new(Mutex::new(runtime_manager));
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
        let agent_code = fs::read_to_string(&entry_point_path).map_err(BamlRtError::Io)?;
        // Execute agent code - this should set up functions on globalThis
        if let Err(e) = js_bridge.evaluate(None, &agent_code).await {
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
