// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Project-level `agentium.toml` configuration.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const DEFAULT_RUNNER_URL: &str = "http://127.0.0.1:18080";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentiumConfig {
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub project: ProjectConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    pub url: String,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub token_file: Option<String>,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            url: DEFAULT_RUNNER_URL.to_string(),
            token_env: Some("RUNNER_TOKEN".to_string()),
            token_file: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub default_agent: Option<String>,
    pub agent_path: Option<String>,
}

impl AgentiumConfig {
    pub fn repository_url(&self) -> String {
        format!("{}/repository", self.runner.url.trim_end_matches('/'))
    }

    pub fn runner_base_url(&self) -> &str {
        self.runner.url.trim_end_matches('/')
    }

    pub fn default_agent_path(&self, cwd: &Path) -> PathBuf {
        if let Some(ref path) = self.project.agent_path {
            return cwd.join(path);
        }
        cwd.to_path_buf()
    }
}

pub fn discover_config(explicit: Option<&Path>) -> Result<(AgentiumConfig, Option<PathBuf>)> {
    if let Some(path) = explicit {
        let cfg = load_file(path)?;
        return Ok((cfg, Some(path.to_path_buf())));
    }
    let local = PathBuf::from("agentium.toml");
    if local.is_file() {
        let cfg = load_file(&local)?;
        return Ok((cfg, Some(local)));
    }
    if let Some(home) = dirs::home_dir() {
        let global = home.join(".config/agentium/config.toml");
        if global.is_file() {
            let cfg = load_file(&global)?;
            return Ok((cfg, Some(global)));
        }
    }
    Ok((AgentiumConfig::default(), None))
}

pub fn load_file(path: &Path) -> Result<AgentiumConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))
}

pub fn write_file(path: &Path, config: &AgentiumConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = toml::to_string_pretty(config).context("Failed to serialize agentium.toml")?;
    std::fs::write(path, raw).with_context(|| format!("Failed to write {}", path.display()))
}

pub fn init_project(dir: &Path, runner_url: &str, agent_name: Option<&str>) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let config_path = dir.join("agentium.toml");
    if config_path.exists() {
        bail!("{} already exists", config_path.display());
    }
    let mut config = AgentiumConfig::default();
    config.runner.url = runner_url.trim_end_matches('/').to_string();
    if let Some(name) = agent_name {
        config.project.default_agent = Some(name.to_string());
        config.project.agent_path = Some(format!("./{name}"));
    }
    write_file(&config_path, &config)?;
    Ok(config_path)
}
