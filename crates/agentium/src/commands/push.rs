// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `push` subcommand — publish and deploy one or more agents sequentially.

use std::{collections::HashSet, path::Path};

use anyhow::{Result, bail};
use console::style;

use super::{
    publish::PublishOriginArg,
    utils::{AgentPlatform, RunnerToken},
};

fn dedupe_agents(agents: &[String]) -> (Vec<String>, Vec<String>) {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    let mut duplicates = Vec::new();

    for agent in agents {
        if seen.insert(agent.clone()) {
            deduped.push(agent.clone());
        } else {
            duplicates.push(agent.clone());
        }
    }

    (deduped, duplicates)
}

fn preflight_validate_agents(agents: &[String]) -> Vec<String> {
    let mut errors = Vec::new();

    for agent_dir in agents {
        let path = Path::new(agent_dir);
        if !path.exists() {
            errors.push(format!("{agent_dir}: path does not exist"));
            continue;
        }
        if !path.is_dir() {
            errors.push(format!("{agent_dir}: path is not a directory"));
            continue;
        }
        if !path.join("manifest.json").is_file() {
            errors.push(format!("{agent_dir}: missing manifest.json"));
        }
        if !path.join("baml_src").is_dir() {
            errors.push(format!("{agent_dir}: missing baml_src/ directory"));
        }
    }

    errors
}

pub fn run(
    agents: &[String],
    repository_url: &str,
    rationale: &str,
    origin: PublishOriginArg,
    url: &str,
    runner_token: Option<RunnerToken>,
) -> Result<()> {
    if agents.is_empty() {
        bail!("At least one agent directory is required. Pass --agents <dir1,dir2,...>.");
    }

    let (agents, duplicates) = dedupe_agents(agents);
    if !duplicates.is_empty() {
        println!(
            "{} duplicate agent directories were skipped: {}",
            style("Warning:").yellow().bold(),
            duplicates.join(", ")
        );
    }

    let preflight_errors = preflight_validate_agents(&agents);
    if !preflight_errors.is_empty() {
        let details = preflight_errors
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "Push preflight failed with {} validation error(s):\n{}",
            preflight_errors.len(),
            details
        );
    }

    let authenticated = runner_token.is_some();
    let http = AgentPlatform::new(runner_token)?;
    let mut success_count = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (index, agent_dir) in agents.iter().enumerate() {
        println!(
            "{}",
            style(format!(
                "[{}/{}] Pushing {}",
                index + 1,
                agents.len(),
                agent_dir
            ))
            .bold()
        );

        let published = match http.publish_agent(agent_dir, repository_url, rationale, origin) {
            Ok(result) => result,
            Err(err) => {
                let msg = format!("publish failed for {agent_dir}: {err:#}");
                println!("  {} {}", style("error:").red().bold(), msg);
                failures.push(msg);
                println!();
                continue;
            }
        };

        let version = format!(
            "{}@v{}",
            published.result.version_ref.name, published.result.version_ref.version
        );
        let hash = published.result.hash.as_str().to_string();
        println!(
            "  {} {}",
            style("published:").green(),
            style(&version).cyan()
        );
        println!("  {} {}", style("hash:").green(), style(&hash).cyan());

        match http.deploy_hash(&hash, url) {
            Ok(deployment) => {
                if deployment.already_deployed {
                    println!(
                        "  {} {}",
                        style("deployed:").yellow(),
                        style("already active").yellow()
                    );
                } else {
                    println!("  {} {}", style("deployed:").green(), style("ok").green());
                }
                success_count += 1;
            }
            Err(err) => {
                let retry_cmd = if authenticated {
                    format!(
                        "cargo agent-platform deploy --hash {hash} --url {url} --runner-token \"$RUNNER_TOKEN\""
                    )
                } else {
                    format!("cargo agent-platform deploy --hash {hash} --url {url}")
                };
                let msg = format!(
                    "deploy failed after successful publish for {version} (hash: {hash}). \
                     Cause: {err:#}. Published artifact was NOT rolled back. \
                     Retry deploy with: {retry_cmd}"
                );
                println!("  {} {}", style("error:").red().bold(), msg);
                failures.push(msg);
            }
        }

        println!();
    }

    println!("{}", style("Push report").bold());
    println!("  successful: {}", style(success_count).green());
    println!("  failed:     {}", style(failures.len()).red());

    if failures.is_empty() {
        println!(
            "{}",
            style(format!(
                "Push complete. Published and deployed {} agent(s).",
                success_count
            ))
            .green()
            .bold()
        );
        return Ok(());
    }

    println!();
    println!("{}", style("Failure details:").red().bold());
    for (i, failure) in failures.iter().enumerate() {
        println!("  {}. {}", i + 1, failure);
    }

    bail!(
        "Push completed with {} successful and {} failed agent(s).",
        success_count,
        failures.len()
    )
}
