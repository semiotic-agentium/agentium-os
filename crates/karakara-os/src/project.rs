// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Project-level `karakara.toml` configuration.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use baml_rt_tools::external_tools::CapabilityRequest;
use semver::Version;
use serde::{Deserialize, Serialize};

const DEFAULT_RUNNER_URL: &str = "http://127.0.0.1:18080";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KarakaraConfig {
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub project: ProjectConfig,
    /// Toolset metadata for an MCP server project. It stays next to
    /// `package.json`, rather than leaking capabilities into individual tools.
    #[serde(default)]
    pub toolset: Option<ToolsetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsetConfig {
    pub name: String,
    pub version: Version,
    #[serde(default)]
    pub capabilities: CapabilityRequest,
    /// Placement for a `Trusted` toolset (external tools v2 §P5.3/P5.5).
    #[serde(default)]
    pub trusted: Option<TrustedSection>,
}

/// `[toolset.trusted]` in `karakara.toml`: where a `Trusted` toolset's
/// supervisor runs. Absent ⇒ the runner spawns its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrustedSection {
    /// The OS spawns the supervisor on `node` (default: local).
    Spawn { node: Option<String> },
    /// The operator runs the supervisor; the OS attaches to `socket_path`.
    Attach {
        socket_path: PathBuf,
        node: Option<String>,
    },
}

impl From<&TrustedSection> for baml_rt_tools::external_tools::KtoolTrustedLaunch {
    fn from(section: &TrustedSection) -> Self {
        match section {
            TrustedSection::Spawn { node } => Self::Spawn { node: node.clone() },
            TrustedSection::Attach { socket_path, node } => Self::Attach {
                socket_path: socket_path.clone(),
                node: node.clone(),
            },
        }
    }
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

impl KarakaraConfig {
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

pub fn discover_config(explicit: Option<&Path>) -> Result<(KarakaraConfig, Option<PathBuf>)> {
    if let Some(path) = explicit {
        let cfg = load_file(path)?;
        return Ok((cfg, Some(path.to_path_buf())));
    }
    let local = PathBuf::from("karakara.toml");
    if local.is_file() {
        let cfg = load_file(&local)?;
        return Ok((cfg, Some(local)));
    }
    if let Some(home) = dirs::home_dir() {
        let global = home.join(".config/karakara/config.toml");
        if global.is_file() {
            let cfg = load_file(&global)?;
            return Ok((cfg, Some(global)));
        }
    }
    Ok((KarakaraConfig::default(), None))
}

pub fn load_file(path: &Path) -> Result<KarakaraConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))
}

/// Read the mandatory toolset declaration from a tool project's `karakara.toml`.
pub fn load_toolset_config(dir: &Path) -> Result<ToolsetConfig> {
    let path = dir.join("karakara.toml");
    let config = load_file(&path)?;
    config.toolset.ok_or_else(|| {
        anyhow::anyhow!(
            "{} is missing [toolset]. Run `karak tool new` or add toolset name, version, and capabilities.",
            path.display()
        )
    })
}

pub fn write_file(path: &Path, config: &KarakaraConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = toml::to_string_pretty(config).context("Failed to serialize karakara.toml")?;
    std::fs::write(path, raw).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolset_capabilities_parse_from_project_toml() {
        let config: KarakaraConfig = toml::from_str(
            r#"
[toolset]
name = "support"
version = "1.2.3"

[[toolset.capabilities.net]]
host = "api.example.test"
port = 443

[[toolset.capabilities.secrets]]
name = "API_TOKEN"
binding = "egress_bound"
"#,
        )
        .unwrap();
        let toolset = config.toolset.unwrap();
        assert_eq!(toolset.name, "support");
        assert_eq!(toolset.version, Version::new(1, 2, 3));
        assert_eq!(toolset.capabilities.net[0].host, "api.example.test");
        assert_eq!(toolset.capabilities.secrets[0].name, "API_TOKEN");
    }

    #[test]
    fn trusted_attach_placement_parses_from_project_toml() {
        let config: KarakaraConfig = toml::from_str(
            r#"
[toolset]
name = "gpu"
version = "0.1.0"

[toolset.trusted]
mode = "attach"
socket_path = "/run/karakara/gpu.sock"
node = "gpu-1"
"#,
        )
        .unwrap();
        let trusted = config.toolset.unwrap().trusted.unwrap();
        assert_eq!(
            trusted,
            TrustedSection::Attach {
                socket_path: PathBuf::from("/run/karakara/gpu.sock"),
                node: Some("gpu-1".into()),
            }
        );
        // The bundler and the host must agree on the placement shape.
        assert!(matches!(
            baml_rt_tools::external_tools::KtoolTrustedLaunch::from(&trusted),
            baml_rt_tools::external_tools::KtoolTrustedLaunch::Attach { ref node, .. }
                if node.as_deref() == Some("gpu-1")
        ));
    }

    #[test]
    fn trusted_spawn_placement_parses_without_a_node() {
        let config: KarakaraConfig = toml::from_str(
            r#"
[toolset]
name = "native"
version = "0.1.0"

[toolset.trusted]
mode = "spawn"
"#,
        )
        .unwrap();
        assert_eq!(
            config.toolset.unwrap().trusted.unwrap(),
            TrustedSection::Spawn { node: None }
        );
    }
}

pub fn init_project(dir: &Path, runner_url: &str, agent_name: Option<&str>) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let config_path = dir.join("karakara.toml");
    if config_path.exists() {
        bail!("{} already exists", config_path.display());
    }
    let mut config = KarakaraConfig::default();
    config.runner.url = runner_url.trim_end_matches('/').to_string();
    if let Some(name) = agent_name {
        config.project.default_agent = Some(name.to_string());
        config.project.agent_path = Some(format!("./{name}"));
    }
    write_file(&config_path, &config)?;
    Ok(config_path)
}
