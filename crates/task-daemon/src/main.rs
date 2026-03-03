use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow};
use baml_task_daemon::{
    A2aSink, ClickUpSink, ExtractionMode, GithubIssueSink, JsonlFileSink, ProjectContext,
    SlackChannelSelector, SlackSourceConfig, SlackTaskSource, StateStore, StdoutSink, TaskDaemon,
    TaskExtractor, TaskSink,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use integrations_slack_read::SlackAuthPreference;
use serde::Deserialize;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SlackAuthMode {
    Auto,
    Bot,
    User,
}

impl From<SlackAuthMode> for SlackAuthPreference {
    fn from(value: SlackAuthMode) -> Self {
        match value {
            SlackAuthMode::Auto => SlackAuthPreference::Auto,
            SlackAuthMode::Bot => SlackAuthPreference::Bot,
            SlackAuthMode::User => SlackAuthPreference::User,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExtractorMode {
    Heuristic,
    Llm,
}

impl From<ExtractorMode> for ExtractionMode {
    fn from(value: ExtractorMode) -> Self {
        match value {
            ExtractorMode::Heuristic => ExtractionMode::Heuristic,
            ExtractorMode::Llm => ExtractionMode::Llm,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "baml-task-daemon",
    about = "Local polling daemon that interprets Slack project discussions into investigation tasks"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
}

#[derive(Debug, Clone, Args)]
struct RunArgs {
    /// Channel selector (`agentium-eng`, `#agentium-eng`, or `C...`).
    #[arg(long, default_value = "agentium-eng")]
    channel: String,

    /// Poll interval in seconds for watch mode.
    /// This delay applies between all polls, including multi-poll Slack backfill.
    #[arg(long, default_value_t = 120)]
    interval_seconds: u64,

    /// Maximum messages requested per Slack history call.
    #[arg(long, default_value_t = 200)]
    history_limit: u16,

    /// Maximum pages to fetch per polling cycle.
    #[arg(long, default_value_t = 3)]
    max_pages: u16,

    /// Lookback window for first run when no cursor exists.
    #[arg(long, default_value_t = 86_400)]
    initial_lookback_seconds: u64,

    /// Path to persisted daemon state (cursor + dedupe keys).
    #[arg(long, default_value = ".agentium/task-daemon-state.json")]
    state_file: PathBuf,

    /// Max seen task keys retained per source.
    #[arg(long, default_value_t = 5_000)]
    max_seen_tasks_per_source: usize,

    /// Run a single poll then exit.
    #[arg(long, default_value_t = false)]
    once: bool,

    /// Emit empty batches (no derived tasks) to configured sinks.
    #[arg(long, default_value_t = false)]
    emit_empty: bool,

    /// Disable stdout sink.
    #[arg(long, default_value_t = false)]
    no_stdout: bool,

    /// Pretty-print JSON output for stdout sink.
    #[arg(long, default_value_t = false)]
    pretty: bool,

    /// Optional JSONL output path for downstream tooling.
    #[arg(long)]
    jsonl_out: Option<PathBuf>,

    /// Max investigation prompts/tasks emitted per poll cycle.
    #[arg(long, default_value_t = 20)]
    max_candidates: usize,

    /// Extraction backend (`llm` default, `heuristic` explicit fallback).
    #[arg(long, value_enum, default_value_t = ExtractorMode::Llm)]
    extractor: ExtractorMode,

    /// Slack auth preference.
    #[arg(long, value_enum, default_value_t = SlackAuthMode::Auto)]
    auth: SlackAuthMode,

    /// Optional workspace URL for deriving message permalinks.
    #[arg(long)]
    workspace_url: Option<String>,

    /// Optional project metadata config (`channel -> project/repo/clickup`).
    #[arg(long, default_value = ".agentium/task-daemon-projects.json")]
    project_config: PathBuf,

    /// Override project key for this channel.
    #[arg(long)]
    project_key: Option<String>,

    /// Optional repository path for codebase-aware investigation prompts.
    #[arg(long)]
    repo_path: Option<PathBuf>,

    /// Optional ClickUp list id for investigation task sink.
    #[arg(long)]
    clickup_list_id: Option<String>,

    /// When set with ClickUp list id, writes live tasks instead of dry-run logging.
    #[arg(long, default_value_t = false)]
    clickup_live: bool,

    /// GitHub repository owner for issue creation sink.
    #[arg(long)]
    github_owner: Option<String>,

    /// GitHub repository name for issue creation sink.
    #[arg(long)]
    github_repo: Option<String>,

    /// When set with GitHub owner/repo, writes live issues instead of dry-run logging.
    #[arg(long, default_value_t = false)]
    github_live: bool,

    /// Coordinator agent URL for A2A delegation sink.
    #[arg(long)]
    coordinator_url: Option<String>,

    /// When set with coordinator URL, sends live A2A requests instead of dry-run logging.
    #[arg(long, default_value_t = false)]
    a2a_live: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProjectConfigFile {
    #[serde(default)]
    channels: BTreeMap<String, ProjectConfigEntry>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProjectConfigEntry {
    #[serde(default)]
    project_key: Option<String>,
    #[serde(default)]
    repo_path: Option<String>,
    #[serde(default)]
    clickup_list_id: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args).await,
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn run(args: RunArgs) -> Result<()> {
    let selector = SlackChannelSelector::parse(&args.channel)
        .map_err(|err| anyhow!("invalid --channel value {}: {err}", args.channel))?;

    let workspace_url = args.workspace_url.clone().or_else(|| {
        std::env::var("SLACK_WORKSPACE_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
    });

    let source = SlackTaskSource::new(SlackSourceConfig {
        channel: selector.clone(),
        history_limit: args.history_limit,
        max_pages: args.max_pages,
        auth_preference: args.auth.into(),
        initial_lookback_seconds: args.initial_lookback_seconds,
        workspace_url,
    });

    let config_entry = load_project_config_entry(&args.project_config, &args.channel, &selector)?;
    let project_context = resolve_project_context(&args, &selector, config_entry.as_ref());

    let mut sinks: Vec<Box<dyn TaskSink>> = Vec::new();
    if !args.no_stdout {
        sinks.push(Box::new(StdoutSink::new(args.pretty)));
    }

    if let Some(path) = args.jsonl_out {
        sinks.push(Box::new(JsonlFileSink::new(path)));
    }

    let clickup_list_id = args
        .clickup_list_id
        .clone()
        .or_else(|| {
            config_entry
                .as_ref()
                .and_then(|entry| entry.clickup_list_id.clone())
        })
        .or_else(|| std::env::var("CLICKUP_TASK_DAEMON_LIST_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(list_id) = clickup_list_id {
        sinks.push(Box::new(ClickUpSink::new(list_id, !args.clickup_live)?));
    }

    if let (Some(owner), Some(repo)) = (args.github_owner.clone(), args.github_repo.clone()) {
        sinks.push(Box::new(GithubIssueSink::new(
            owner,
            repo,
            !args.github_live,
        )?));
    }

    if let Some(url) = args.coordinator_url.clone() {
        sinks.push(Box::new(A2aSink::new(url, !args.a2a_live)?));
    }

    if sinks.is_empty() {
        return Err(anyhow!(
            "no sinks configured; enable stdout, --jsonl-out, --clickup-list-id, --github-owner/--github-repo, or --coordinator-url"
        ));
    }

    let state_store = StateStore::new(args.state_file, args.max_seen_tasks_per_source);
    let extractor = TaskExtractor::with_mode(args.max_candidates, args.extractor.into())?;
    let mut daemon = TaskDaemon::new(
        Box::new(source),
        extractor,
        sinks,
        state_store,
        project_context,
    );
    daemon.set_emit_empty_batches(args.emit_empty);

    if args.once {
        let batch = daemon.run_once().await?;
        tracing::info!(
            source = %batch.source_label,
            derived_tasks = batch.derived_tasks.len(),
            messages_scanned = batch.messages_scanned,
            project_key = %batch.project.project_key,
            "task-daemon run-once completed"
        );
        return Ok(());
    }

    daemon
        .run_loop(Duration::from_secs(args.interval_seconds.max(1)))
        .await
}

fn resolve_project_context(
    args: &RunArgs,
    selector: &SlackChannelSelector,
    entry: Option<&ProjectConfigEntry>,
) -> ProjectContext {
    let repo_path = args
        .repo_path
        .as_ref()
        .map(|path| path.display().to_string())
        .or_else(|| entry.and_then(|entry| entry.repo_path.clone()))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let project_key = args
        .project_key
        .clone()
        .or_else(|| entry.and_then(|entry| entry.project_key.clone()))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("slack-{}", selector.state_fragment().to_ascii_lowercase()));

    ProjectContext {
        project_key,
        repo_available: repo_path.is_some(),
        repo_path,
    }
}

fn load_project_config_entry(
    config_path: &PathBuf,
    channel_raw: &str,
    selector: &SlackChannelSelector,
) -> Result<Option<ProjectConfigEntry>> {
    if !config_path.exists() {
        return Ok(None);
    }

    let bytes = std::fs::read(config_path)
        .with_context(|| format!("reading project config at {}", config_path.display()))?;
    let config: ProjectConfigFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing project config at {}", config_path.display()))?;

    for key in project_lookup_keys(channel_raw, selector) {
        if let Some(entry) = config.channels.get(&key) {
            return Ok(Some(entry.clone()));
        }
    }

    Ok(None)
}

fn project_lookup_keys(channel_raw: &str, selector: &SlackChannelSelector) -> Vec<String> {
    let mut keys = Vec::new();
    let raw_trimmed = channel_raw.trim();
    if !raw_trimmed.is_empty() {
        keys.push(raw_trimmed.to_string());
        keys.push(raw_trimmed.trim_start_matches('#').to_ascii_lowercase());
    }

    match selector {
        SlackChannelSelector::ChannelName(name) => {
            keys.push(name.to_ascii_lowercase());
            keys.push(format!("#{name}"));
        }
        SlackChannelSelector::ChannelId(id) => {
            keys.push(id.to_ascii_uppercase());
        }
    }

    keys.sort();
    keys.dedup();
    keys
}
