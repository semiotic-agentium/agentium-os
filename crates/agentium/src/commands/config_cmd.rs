// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `agentium config show|set`

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use console::style;

use crate::project::{self, AgentiumConfig};

pub fn show(config_path: Option<&Path>) -> Result<()> {
    let (cfg, loaded_from) = project::discover_config(config_path)?;
    if let Some(path) = loaded_from {
        println!("{} {}", style("config:").dim(), path.display());
    } else {
        println!("{} (defaults)", style("config:").dim());
    }
    println!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}

pub fn set(key: &str, value: &str, config_path: Option<&Path>) -> Result<()> {
    let path = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("agentium.toml"));
    let mut cfg = if path.is_file() {
        project::load_file(&path)?
    } else {
        AgentiumConfig::default()
    };

    match key {
        "runner.url" => cfg.runner.url = value.trim_end_matches('/').to_string(),
        "project.default_agent" => cfg.project.default_agent = Some(value.to_string()),
        "project.agent_path" => cfg.project.agent_path = Some(value.to_string()),
        "runner.token_env" => cfg.runner.token_env = Some(value.to_string()),
        other => bail!("Unknown config key: {other}"),
    }

    project::write_file(&path, &cfg)?;
    println!(
        "{} Updated {} = {}",
        style("[config]").green(),
        style(key).cyan(),
        value
    );
    Ok(())
}
