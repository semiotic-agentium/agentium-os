//! `undeploy` subcommand — remove an active deployed package hash from a running runner.

use anyhow::{Result, bail};
use console::style;
use serde::{Deserialize, Serialize};

use super::utils::{AgentPlatform, HTTP_OP_UNDEPLOY, join_url};

#[derive(Debug, Serialize)]
struct UndeployRequest<'a> {
    hash: &'a str,
}

#[derive(Debug, Deserialize)]
struct UndeployResponse {
    removed: bool,
}

pub struct UndeployOutput {
    pub removed: bool,
    pub undeploy_url: String,
}

impl AgentPlatform {
    pub fn undeploy_hash(&self, hash: &str, base_url: &str) -> Result<UndeployOutput> {
        if hash.trim().is_empty() {
            bail!("hash must not be empty");
        }

        let undeploy_url = join_url(base_url, "/undeploy");
        let payload = UndeployRequest { hash };
        let response: UndeployResponse =
            self.post_json(&undeploy_url, &payload, HTTP_OP_UNDEPLOY)?;

        Ok(UndeployOutput {
            removed: response.removed,
            undeploy_url,
        })
    }
}

pub fn undeploy_hash(hash: &str, base_url: &str) -> Result<UndeployOutput> {
    let http = AgentPlatform::new()?;
    http.undeploy_hash(hash, base_url)
}

pub fn run(hash: &str, base_url: &str) -> Result<()> {
    let result = undeploy_hash(hash, base_url)?;

    if result.removed {
        println!("{}", style("Undeploy successful.").green().bold());
    } else {
        println!("{}", style("No active deployment removed.").yellow().bold());
    }
    println!("  hash: {}", style(hash).cyan());
    println!("  url:  {}", style(result.undeploy_url).dim());

    Ok(())
}
