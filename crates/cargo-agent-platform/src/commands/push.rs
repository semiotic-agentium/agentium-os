//! `push` subcommand — publish and deploy one or more agents sequentially.

use anyhow::{Context, Result, bail};
use console::style;

use super::{publish::PublishOriginArg, utils::AgentPlatform};

pub fn run(
    agents: &[String],
    repository_url: &str,
    rationale: &str,
    origin: PublishOriginArg,
    url: &str,
) -> Result<()> {
    if agents.is_empty() {
        bail!("At least one agent directory is required. Pass --agents <dir1,dir2,...>.");
    }

    let http = AgentPlatform::new()?;
    let mut deployed_hashes: Vec<String> = Vec::new();

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

        let published = http
            .publish_agent(agent_dir, repository_url, rationale, origin)
            .with_context(|| format!("Failed to publish agent directory: {agent_dir}"))?;

        let version = format!(
            "{}@v{}",
            published.result.version_ref.name, published.result.version_ref.version
        );
        println!(
            "  {} {}",
            style("published:").green(),
            style(version).cyan()
        );
        println!(
            "  {} {}",
            style("hash:").green(),
            style(published.result.hash.as_str()).cyan()
        );

        let deployment = http
            .deploy_hash(published.result.hash.as_str(), url)
            .with_context(|| format!("Failed to deploy hash {}", published.result.hash.as_str()))?;

        if deployment.already_deployed {
            println!(
                "  {} {}",
                style("deployed:").yellow(),
                style("already active").yellow()
            );
        } else {
            println!("  {} {}", style("deployed:").green(), style("ok").green());
        }

        deployed_hashes.push(deployment.hash);
        println!();
    }

    println!(
        "{}",
        style(format!(
            "Push complete. Published and deployed {} agent(s).",
            deployed_hashes.len()
        ))
        .green()
        .bold()
    );

    Ok(())
}
