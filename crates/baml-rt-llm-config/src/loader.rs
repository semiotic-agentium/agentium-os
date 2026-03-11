//! Load LlmClientConfig from file (YAML/JSON) or env.

use std::{collections::HashMap, path::Path};

use anyhow::Result;
use serde::Deserialize;

use crate::config::{ClientDef, LlmClientConfig, LlmOverrides, RetryPolicyDef};

/// Raw config as deserialized from YAML/JSON (optional default).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RawConfig {
    default: Option<String>,
    clients: Option<Vec<ClientDef>>,
    overrides: Option<LlmOverrides>,
    retry_policies: Option<HashMap<String, RetryPolicyDef>>,
}

impl LlmClientConfig {
    /// Load config from a file path (YAML or JSON by extension).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let s = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read llm config {}: {}", path.display(), e))?;
        Self::load_from_str(&s, path.extension().and_then(|e| e.to_str()))
    }

    /// Load config from string (format hint: Some("yaml") or Some("json")).
    pub fn load_from_str(s: &str, format_hint: Option<&str>) -> Result<Self> {
        let raw: RawConfig = match format_hint {
            Some("yaml") | Some("yml") => serde_yaml::from_str(s)
                .map_err(|e| anyhow::anyhow!("parse llm config yaml: {}", e))?,
            _ => {
                // Try JSON first, then YAML
                serde_json::from_str(s)
                    .or_else(|_| serde_yaml::from_str(s))
                    .map_err(|e| anyhow::anyhow!("parse llm config: {}", e))?
            }
        };
        let default = raw.default.unwrap_or_default();
        let clients: HashMap<String, ClientDef> = raw
            .clients
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.name.clone(), c))
            .collect();
        let overrides = raw.overrides.unwrap_or_default();
        let retry_policies = raw.retry_policies.unwrap_or_default();
        let mut config = Self {
            default,
            clients,
            overrides,
            retry_policies,
        };
        config.normalize();
        Ok(config)
    }
}
