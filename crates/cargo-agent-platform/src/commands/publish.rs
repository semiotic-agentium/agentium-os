//! `publish` subcommand — publish an agent source bundle.

use std::path::Path;

use anyhow::{Context, Result, bail};
use baml_rt_repository::{
    commands::{PublishCommand, PublishOrigin, PublishResult},
    entry::ChangeRationale,
    source_bundle_from_agent_dir,
};
use clap::ValueEnum;
use console::style;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum PublishOriginArg {
    Original,
    Iteration,
}

impl From<PublishOriginArg> for PublishOrigin {
    fn from(value: PublishOriginArg) -> Self {
        match value {
            PublishOriginArg::Original => PublishOrigin::Original,
            PublishOriginArg::Iteration => PublishOrigin::Iteration,
        }
    }
}

/// Publish an agent source directory.
///
/// Flow:
/// 1. Read source bundle from `agent_dir`
/// 2. `POST {repository_url}/publish` with `PublishCommand`
pub fn run(
    agent_dir: &str,
    repository_url: &str,
    rationale: &str,
    origin: PublishOriginArg,
) -> Result<()> {
    let agent_dir = Path::new(agent_dir);
    if !agent_dir.exists() {
        bail!("Agent directory not found: {}", agent_dir.display());
    }
    if !agent_dir.is_dir() {
        bail!("Agent path is not a directory: {}", agent_dir.display());
    }

    let (name, source) =
        source_bundle_from_agent_dir(agent_dir).context("Invalid agent source directory")?;
    let rationale = ChangeRationale::new(rationale.to_string()).context("Invalid rationale")?;
    let origin: PublishOrigin = origin.into();
    let publish_cmd = PublishCommand {
        name,
        source,
        rationale,
        origin,
    };

    let base = repository_url.trim_end_matches('/');
    let publish_url = format!("{base}/publish");

    let rt = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
    let publish_result: PublishResult = rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(&publish_url)
            .header("content-type", "application/json")
            .json(&publish_cmd)
            .send()
            .await
            .with_context(|| format!("Failed to POST publish to {publish_url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Publish failed ({status}) at {publish_url}: {body}");
        }

        serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse publish response: {body}"))
    })?;

    let content_hash = publish_result.hash.as_str();
    println!("{}", style("Source published successfully.").green().bold());
    println!("  agent dir: {}", style(agent_dir.display()).cyan());
    println!("  url:       {}", style(&publish_url).dim());
    println!(
        "  version:   {}",
        style(format!(
            "{}@v{}",
            publish_result.version_ref.name, publish_result.version_ref.version
        ))
        .cyan()
    );
    println!("  hash:      {}", style(content_hash).cyan());
    println!();
    println!("{}", style("To deploy this agent, run:").yellow());
    println!(
        "  cargo agent-platform deploy --hash {}",
        style(content_hash).cyan()
    );

    Ok(())
}
