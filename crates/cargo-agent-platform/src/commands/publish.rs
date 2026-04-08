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

use super::utils::{AgentPlatform, join_url};

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

pub struct PublishOutput {
    pub publish_url: String,
    pub result: PublishResult,
}

impl AgentPlatform {
    pub fn publish_agent(
        &self,
        agent_dir: &str,
        repository_url: &str,
        rationale: &str,
        origin: PublishOriginArg,
    ) -> Result<PublishOutput> {
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

        let publish_url = join_url(repository_url, "/publish");
        let publish_result: PublishResult =
            self.post_json(&publish_url, &publish_cmd, "Publish")?;

        Ok(PublishOutput {
            publish_url,
            result: publish_result,
        })
    }
}

pub fn publish_agent(
    agent_dir: &str,
    repository_url: &str,
    rationale: &str,
    origin: PublishOriginArg,
) -> Result<PublishOutput> {
    let http = AgentPlatform::new()?;
    http.publish_agent(agent_dir, repository_url, rationale, origin)
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
    let published = publish_agent(agent_dir, repository_url, rationale, origin)?;
    let content_hash = published.result.hash.as_str();

    println!("{}", style("Source published successfully.").green().bold());
    println!("  agent dir: {}", style(agent_dir).cyan());
    println!("  url:       {}", style(&published.publish_url).dim());
    println!(
        "  version:   {}",
        style(format!(
            "{}@v{}",
            published.result.version_ref.name, published.result.version_ref.version
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
