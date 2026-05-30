// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Load LlmClientConfig from file (YAML/JSON) or env.

use std::{collections::HashMap, path::Path};

use anyhow::{Context as _, Result};
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
            .with_context(|| format!("read llm config: {}", path.display()))?;
        Self::load_from_str(&s, path.extension().and_then(|e| e.to_str()))
    }

    /// Load config from string (format hint: Some("yaml") or Some("json")).
    pub fn load_from_str(s: &str, format_hint: Option<&str>) -> Result<Self> {
        let raw: RawConfig = match format_hint {
            Some("yaml") | Some("yml") => {
                serde_yaml::from_str(s).context("parse llm config yaml")?
            }
            _ => {
                // Try JSON first, then YAML; capture the JSON error for richer diagnostics.
                serde_json::from_str(s)
                    .or_else(|json_err| {
                        serde_yaml::from_str(s).with_context(|| {
                            format!("parse llm config (json failed: {json_err}; yaml also failed)")
                        })
                    })
                    .context("parse llm config")?
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
