// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak install agent` — publish agent source and deploy it.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use console::style;

use super::{publish::PublishOriginArg, push::run as push_run, utils::RunnerToken};
use crate::project::{self, KarakaraConfig};

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

fn resolve_agent_path(path: Option<&str>, cfg: &KarakaraConfig, cwd: &Path) -> Result<PathBuf> {
    if let Some(p) = path {
        let pb = PathBuf::from(p);
        return Ok(if pb.is_absolute() { pb } else { cwd.join(pb) });
    }
    Ok(cfg.default_agent_path(cwd))
}
