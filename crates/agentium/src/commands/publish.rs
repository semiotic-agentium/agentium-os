// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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

use super::utils::{AgentPlatform, HTTP_OP_PUBLISH, RunnerToken, join_url};

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
            self.post_json(&publish_url, &publish_cmd, HTTP_OP_PUBLISH)?;

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
    runner_token: Option<RunnerToken>,
) -> Result<PublishOutput> {
    let http = AgentPlatform::new(runner_token)?;
    http.publish_agent(agent_dir, repository_url, rationale, origin)
}

pub fn run(
    agent_dir: &str,
    repository_url: &str,
    rationale: &str,
    origin: PublishOriginArg,
    runner_token: Option<RunnerToken>,
) -> Result<()> {
    let authenticated = runner_token.is_some();
    let published = publish_agent(agent_dir, repository_url, rationale, origin, runner_token)?;
    let content_hash = published.result.hash.as_str();
    let runner_url = repository_url
        .trim_end_matches('/')
        .strip_suffix("/repository")
        .unwrap_or(repository_url.trim_end_matches('/'));

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
    if authenticated {
        println!(
            "  agentium deploy --hash {} --url {} --runner-token \"$RUNNER_TOKEN\"",
            style(content_hash).cyan(),
            style(runner_url).dim()
        );
    } else {
        println!(
            "  agentium deploy --hash {} --url {}",
            style(content_hash).cyan(),
            style(runner_url).dim()
        );
    }

    Ok(())
}
