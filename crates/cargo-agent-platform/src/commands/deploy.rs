//! `deploy` subcommand — activate a published package hash in a running runner.

use anyhow::{Context, Result, bail};
use console::style;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct DeployRequest<'a> {
    hash: &'a str,
}

#[derive(Debug, Deserialize)]
struct DeployResponse {
    hash: String,
    already_deployed: bool,
}

pub fn run(hash: &str, base_url: &str) -> Result<()> {
    if hash.trim().is_empty() {
        bail!("hash must not be empty");
    }

    let base = base_url.trim_end_matches('/');
    let deploy_url = format!("{base}/deploy");
    let payload = DeployRequest { hash };

    let rt = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
    let response = rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = client
            .post(&deploy_url)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to POST deploy to {deploy_url}"))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Deploy failed ({status}) at {deploy_url}: {body}");
        }

        serde_json::from_str::<DeployResponse>(&body)
            .with_context(|| format!("Invalid deploy response JSON: {body}"))
    })?;

    if response.already_deployed {
        println!(
            "{}",
            style("Deployment already active (idempotent).")
                .yellow()
                .bold()
        );
    } else {
        println!("{}", style("Deployment successful.").green().bold());
    }
    println!("  hash: {}", style(response.hash).cyan());
    println!("  url:  {}", style(deploy_url).dim());

    Ok(())
}
