//! `list-deployed-instances` subcommand — list currently loaded runner instances.

use anyhow::{Context, Result, bail};
use console::style;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct AgentDiscoveryEntry {
    agent_card: AgentCard,
}

#[derive(Debug, Deserialize)]
struct AgentCard {
    name: String,
    agent_package: String,
    agent_instance_id: String,
}

pub fn run(base_url: &str) -> Result<()> {
    let base = base_url.trim_end_matches('/');
    let agents_url = format!("{base}/agents");

    let rt = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
    let entries = rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .get(&agents_url)
            .send()
            .await
            .with_context(|| format!("Failed to GET deployed instances from {agents_url}"))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("List deployed instances failed ({status}) at {agents_url}: {body}");
        }

        serde_json::from_str::<Vec<AgentDiscoveryEntry>>(&body)
            .with_context(|| format!("Invalid /agents response JSON: {body}"))
    })?;

    if entries.is_empty() {
        println!("{}", style("No deployed instances found.").yellow().bold());
        println!("  url: {}", style(agents_url).dim());
        return Ok(());
    }

    println!("{}", style("Deployed instances").bold());
    for entry in entries {
        println!(
            "- {}  package={} instance={}",
            style(entry.agent_card.name).cyan(),
            entry.agent_card.agent_package,
            entry.agent_card.agent_instance_id
        );
    }
    println!("  url: {}", style(agents_url).dim());

    Ok(())
}
