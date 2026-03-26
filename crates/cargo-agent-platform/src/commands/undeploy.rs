//! `undeploy` subcommand — remove an active deployed package hash from a running runner.

use anyhow::{Context, Result, bail};
use console::style;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct UndeployRequest<'a> {
    hash: &'a str,
}

#[derive(Debug, Deserialize)]
struct UndeployResponse {
    removed: bool,
}

pub fn run(hash: &str, base_url: &str) -> Result<()> {
    if hash.trim().is_empty() {
        bail!("hash must not be empty");
    }

    let base = base_url.trim_end_matches('/');
    let undeploy_url = format!("{base}/undeploy");
    let payload = UndeployRequest { hash };

    let rt = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
    let response = rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(&undeploy_url)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to POST undeploy to {undeploy_url}"))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Undeploy failed ({status}) at {undeploy_url}: {body}");
        }

        serde_json::from_str::<UndeployResponse>(&body)
            .with_context(|| format!("Invalid undeploy response JSON: {body}"))
    })?;

    if response.removed {
        println!("{}", style("Undeploy successful.").green().bold());
    } else {
        println!("{}", style("No active deployment removed.").yellow().bold());
    }
    println!("  hash: {}", style(hash).cyan());
    println!("  url:  {}", style(undeploy_url).dim());

    Ok(())
}
