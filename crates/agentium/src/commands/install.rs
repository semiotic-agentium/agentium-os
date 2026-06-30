// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `agentium install agent|tool` — publish source and deploy (or enable tool).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use console::style;

use super::{publish::PublishOriginArg, push::run as push_run, utils::RunnerToken};
use crate::project::{self, AgentiumConfig};

pub fn install_agent(
    path: Option<&str>,
    config_path: Option<&Path>,
    repository_url: Option<&str>,
    runner_url: Option<&str>,
    rationale: &str,
    origin: PublishOriginArg,
    runner_token: Option<RunnerToken>,
) -> Result<()> {
    let (cfg, _) = project::discover_config(config_path)?;
    let cwd = std::env::current_dir()?;
    let agent_dir = resolve_agent_path(path, &cfg, &cwd)?;

    let repo_url = repository_url
        .map(str::to_string)
        .unwrap_or_else(|| cfg.repository_url());
    let base_url = runner_url
        .unwrap_or(cfg.runner_base_url())
        .trim_end_matches('/')
        .to_string();

    if !agent_dir.join("manifest.json").is_file() {
        bail!(
            "No manifest.json in {} — point --path at an agent directory",
            agent_dir.display()
        );
    }

    println!(
        "{} Installing {} → {}",
        style("[install]").bold().dim(),
        style(agent_dir.display()).cyan(),
        style(&base_url).dim()
    );

    push_run(
        &[agent_dir.to_string_lossy().into_owned()],
        &repo_url,
        rationale,
        origin,
        &base_url,
        runner_token,
    )
}

pub fn install_tool(
    dir: &str,
    repository_url: Option<&str>,
    runner_token: Option<&str>,
    sandbox_rootfs: Option<&str>,
    approved_by: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<()> {
    let (cfg, _) = project::discover_config(None)?;
    let repo = cfg.repository_url();
    let repo_url = repository_url.unwrap_or(&repo);
    super::external_tool::enable(super::external_tool::EnableParams {
        dir,
        repository_url: Some(repo_url),
        runner_token,
        sandbox_rootfs,
        approved_by,
        yes,
        json_output: json,
    })
}

fn resolve_agent_path(path: Option<&str>, cfg: &AgentiumConfig, cwd: &Path) -> Result<PathBuf> {
    if let Some(p) = path {
        let pb = PathBuf::from(p);
        return Ok(if pb.is_absolute() { pb } else { cwd.join(pb) });
    }
    Ok(cfg.default_agent_path(cwd))
}
