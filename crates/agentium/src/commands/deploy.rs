// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `deploy` subcommand — activate a published package hash in a running runner.

use anyhow::{Result, bail};
use console::style;
use serde::{Deserialize, Serialize};

use super::utils::{AgentPlatform, HTTP_OP_DEPLOY, RunnerToken, join_url};

#[derive(Debug, Serialize)]
struct DeployRequest<'a> {
    hash: &'a str,
}

#[derive(Debug, Deserialize)]
struct DeployResponse {
    hash: String,
    already_deployed: bool,
}

pub struct DeployOutput {
    pub hash: String,
    pub already_deployed: bool,
    pub deploy_url: String,
}

impl AgentPlatform {
    pub fn deploy_hash(&self, hash: &str, base_url: &str) -> Result<DeployOutput> {
        if hash.trim().is_empty() {
            bail!("hash must not be empty");
        }

        let deploy_url = join_url(base_url, "/deploy");
        let payload = DeployRequest { hash };
        let response: DeployResponse = self.post_json(&deploy_url, &payload, HTTP_OP_DEPLOY)?;

        Ok(DeployOutput {
            hash: response.hash,
            already_deployed: response.already_deployed,
            deploy_url,
        })
    }
}

pub fn deploy_hash(
    hash: &str,
    base_url: &str,
    runner_token: Option<RunnerToken>,
) -> Result<DeployOutput> {
    let http = AgentPlatform::new(runner_token)?;
    http.deploy_hash(hash, base_url)
}

pub fn run(hash: &str, base_url: &str, runner_token: Option<RunnerToken>) -> Result<()> {
    let deployment = deploy_hash(hash, base_url, runner_token)?;

    if deployment.already_deployed {
        println!(
            "{}",
            style("Deployment already active (idempotent).")
                .yellow()
                .bold()
        );
    } else {
        println!("{}", style("Deployment successful.").green().bold());
    }
    println!("  hash: {}", style(deployment.hash).cyan());
    println!("  url:  {}", style(deployment.deploy_url).dim());

    Ok(())
}
