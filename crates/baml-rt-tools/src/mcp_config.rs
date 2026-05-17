//! Operator-owned `mcp-servers.json` config: declares MCP servers a runner
//! instance may import or run.
//!
//! Accepts the canonical `mcpServers` shape used by Claude Desktop and similar
//! MCP clients, extended with `secrets` and `sandbox` blocks. Raw secret values
//! are not permitted in `env`; declare them in `secrets` and the runtime
//! resolves them through the existing fnox/env chain at child-spawn time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mcp_snapshot::{Digest, canonical_digest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServersFile {
    #[serde(rename = "mcpServers", default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Plain environment variables. Must not carry secret values; use
    /// `secrets` instead.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Required secrets, declared the same way `#[baml_tool]` tools declare
    /// theirs. The runtime resolves each `name` via the configured secret
    /// resolver chain and injects the value into the child process env.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretDecl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxConfig>,
    /// Optional human-readable description shown by MCP registry import commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretDecl {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SandboxConfig {
    /// Sandbox profile name. Defaults to `mcp-import-restricted` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Hard deadline (seconds) for import-time discovery. Defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_timeout_secs: Option<u64>,
    /// Per-call deadline (seconds) for `tools/call` at runtime. Defaults to 120.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_call_timeout_secs: Option<u64>,
}

#[derive(Debug, Error)]
pub enum McpConfigError {
    #[error("invalid mcp-servers.json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(
        "server `{server}` env field `{key}` looks like a secret; declare it under `secrets` instead"
    )]
    SecretInEnv { server: String, key: String },
    #[error("server `{server}` declares secret `{name}` more than once")]
    DuplicateSecret { server: String, name: String },
    #[error(
        "server id `{0}` must be non-empty and contain only ASCII letters, digits, `_`, or `-`"
    )]
    InvalidServerId(String),
    #[error("server `{server}` command is empty")]
    EmptyCommand { server: String },
}

/// Computes the approved launch-config digest for one MCP server.
///
/// Covers launch-shaping fields, secret identities, and sandbox policy, but
/// deliberately excludes raw non-secret env values so host-specific endpoints
/// do not leak into snapshots.
pub fn compute_server_config_digest(server_id: &str, config: &McpServerConfig) -> Digest {
    let canonical = serde_json::json!({
        "server_id": server_id,
        "command": config.command,
        "args": config.args,
        "env_keys": config.env.keys().collect::<Vec<_>>(),
        "secret_names": config.secrets.iter().map(|s| &s.name).collect::<Vec<_>>(),
        "sandbox": config.sandbox,
    });
    canonical_digest(&canonical)
}

impl McpServersFile {
    pub fn parse(input: &str) -> Result<Self, McpConfigError> {
        let parsed: Self = serde_json::from_str(input)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), McpConfigError> {
        for (id, config) in &self.servers {
            if !is_valid_server_id(id) {
                return Err(McpConfigError::InvalidServerId(id.clone()));
            }
            if config.command.trim().is_empty() {
                return Err(McpConfigError::EmptyCommand { server: id.clone() });
            }
            for key in config.env.keys() {
                if looks_like_secret_name(key) {
                    return Err(McpConfigError::SecretInEnv {
                        server: id.clone(),
                        key: key.clone(),
                    });
                }
            }
            let mut seen = std::collections::HashSet::new();
            for secret in &config.secrets {
                if !seen.insert(secret.name.clone()) {
                    return Err(McpConfigError::DuplicateSecret {
                        server: id.clone(),
                        name: secret.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn is_valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Conservative heuristic: env keys containing these tokens are rejected as
/// likely secret carriers. Operators that need a literal env var with one of
/// these names can rename it (e.g. set the secret via the `secrets:` block
/// and reference it from inside the MCP server itself).
fn looks_like_secret_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "ACCESS_KEY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_mcp_servers_shape() {
        let json = r#"{
          "mcpServers": {
            "grafana": {
              "command": "uvx",
              "args": ["mcp-grafana"],
              "env": { "GRAFANA_URL": "http://localhost:3000" },
              "secrets": [
                { "name": "GRAFANA_SERVICE_ACCOUNT_TOKEN", "description": "Grafana SA token" }
              ],
              "sandbox": { "profile": "mcp-import-restricted" }
            }
          }
        }"#;
        let parsed = McpServersFile::parse(json).expect("parse");
        let server = parsed.servers.get("grafana").expect("server present");
        assert_eq!(server.command, "uvx");
        assert_eq!(server.args, vec!["mcp-grafana".to_string()]);
        assert_eq!(
            server.env.get("GRAFANA_URL").map(String::as_str),
            Some("http://localhost:3000")
        );
        assert_eq!(server.secrets.len(), 1);
        assert_eq!(server.secrets[0].name, "GRAFANA_SERVICE_ACCOUNT_TOKEN");
    }

    #[test]
    fn rejects_secret_looking_env_key() {
        let json = r#"{
          "mcpServers": {
            "x": {
              "command": "foo",
              "env": { "FOO_API_KEY": "abc" }
            }
          }
        }"#;
        let err = McpServersFile::parse(json).expect_err("should reject");
        assert!(matches!(err, McpConfigError::SecretInEnv { .. }));
    }

    #[test]
    fn rejects_empty_command() {
        let json = r#"{ "mcpServers": { "x": { "command": "" } } }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::EmptyCommand { .. }));
    }

    #[test]
    fn rejects_invalid_server_id() {
        let json = r#"{ "mcpServers": { "bad id!": { "command": "x" } } }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::InvalidServerId(_)));
    }

    #[test]
    fn rejects_duplicate_secret_name() {
        let json = r#"{
          "mcpServers": {
            "x": {
              "command": "foo",
              "secrets": [
                { "name": "A" },
                { "name": "A" }
              ]
            }
          }
        }"#;
        let err = McpServersFile::parse(json).expect_err("reject");
        assert!(matches!(err, McpConfigError::DuplicateSecret { .. }));
    }

    #[test]
    fn missing_mcp_servers_key_yields_empty_file() {
        let parsed = McpServersFile::parse("{}").expect("empty parse");
        assert!(parsed.servers.is_empty());
    }
}
