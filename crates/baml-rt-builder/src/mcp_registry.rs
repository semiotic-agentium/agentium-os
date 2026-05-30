// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Reusable MCP registry operator commands.
//!
//! Kept in library code so CLIs can enable MCP registry entries without
//! shelling out to `baml-agent-builder`.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use baml_rt_tools::mcp_snapshot::McpServerSnapshot;

/// Options for discovering, approving, and importing one MCP server snapshot.
pub struct McpRegistryEnableOptions<'a> {
    /// Server id to enable. Must match an entry in the MCP servers config.
    pub server_id: &'a str,
    /// Path to mcp-servers.json. Defaults to `$HOME/.agentium-os/mcp-servers.json`.
    pub config_path: Option<&'a Path>,
    /// Repository URL where `/mcp/snapshots/import` is mounted.
    pub repository_url: &'a str,
    /// Skip interactive approval prompt.
    pub skip_prompt: bool,
    /// Runner token. If `None`, falls back to `RUNNER_TOKEN`.
    pub runner_token: Option<&'a str>,
}

/// Discover server tools, approve snapshot, and import into MCP registry.
pub async fn enable_mcp_registry_server(opts: McpRegistryEnableOptions<'_>) -> Result<()> {
    let mut snapshot = import_mcp_snapshot_from_config(opts.server_id, opts.config_path).await?;
    print_mcp_snapshot_summary(&snapshot);
    let approve = if opts.skip_prompt {
        true
    } else {
        inquire::Confirm::new("Approve this server and all tools into the registry?")
            .with_default(false)
            .prompt()
            .unwrap_or(false)
    };
    if !approve {
        println!("Aborted. Registry was not modified.");
        return Ok(());
    }
    approve_mcp_snapshot(&mut snapshot);
    let body = post_mcp_snapshot_to_registry(
        opts.repository_url,
        snapshot,
        opts.runner_token,
        "MCP registry enable",
    )
    .await?;
    let version = body
        .get("version")
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "<unknown>".into());
    println!(
        "✅ Enabled MCP server `{}` as registry version {version}",
        opts.server_id
    );
    Ok(())
}

/// Pure token resolution from explicit sources (testable without env mutation).
pub fn resolve_runner_token_from_sources(
    flag: Option<&str>,
    env_value: Option<String>,
) -> Result<Option<String>> {
    let trimmed = match flag {
        Some(v) => Some(v.trim().to_owned()),
        None => env_value.map(|v| v.trim().to_owned()),
    };
    match trimmed {
        Some(v) if v.is_empty() => {
            bail!(
                "Runner token is empty or whitespace-only. \
                 Provide a valid token via --runner-token or RUNNER_TOKEN."
            );
        }
        Some(v) => Ok(Some(v)),
        None => Ok(None),
    }
}

/// Resolve runner token from CLI flag with `RUNNER_TOKEN` env fallback.
pub fn resolve_runner_token(flag: Option<&str>) -> Result<Option<String>> {
    resolve_runner_token_from_sources(flag, std::env::var("RUNNER_TOKEN").ok())
}

fn check_response(
    status: reqwest::StatusCode,
    body: &str,
    op_name: &str,
    authenticated: bool,
) -> Result<()> {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let hint = if authenticated {
            "Hint: the runner token was rejected \
             — verify it matches the server's RUNNER_TOKEN."
        } else {
            "Hint: pass --runner-token <token> or set the \
             RUNNER_TOKEN environment variable."
        };
        bail!("{op_name} failed ({status}): {body}. {hint}");
    }
    if !status.is_success() {
        bail!("{op_name} failed ({status}): {body}");
    }
    Ok(())
}

async fn post_mcp_snapshot_to_registry(
    repository_url: &str,
    snapshot: McpServerSnapshot,
    runner_token_flag: Option<&str>,
    op_name: &str,
) -> Result<serde_json::Value> {
    use baml_rt_repository::http::ImportMcpSnapshotRequest;

    let token = resolve_runner_token(runner_token_flag)?;
    let url = format!(
        "{}/mcp/snapshots/import",
        repository_url.trim_end_matches('/')
    );
    let http = reqwest::Client::new();
    let mut request = http
        .post(url.as_str())
        .json(&ImportMcpSnapshotRequest { snapshot });
    if let Some(ref token) = token {
        request = request.header("X-Runner-Token", token.as_str());
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("posting MCP snapshot to {url}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    check_response(status, &body, op_name, token.is_some())?;
    serde_json::from_str(&body).context("Failed to parse MCP registry response")
}

fn mcp_default_config_path() -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --config explicitly"))?;
    Ok(home.join(".agentium-os").join("mcp-servers.json"))
}

/// Import one MCP server snapshot from config using live MCP discovery.
pub async fn import_mcp_snapshot_from_config(
    server_id: &str,
    config_path: Option<&Path>,
) -> Result<McpServerSnapshot> {
    use baml_rt_mcp::importer::{EnvSecretResolver, ImportOptions, Importer};
    use baml_rt_tools::mcp_config::McpServersFile;

    let config_path = match config_path {
        Some(p) => p.to_path_buf(),
        None => mcp_default_config_path()?,
    };
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading mcp-servers config at {}", config_path.display()))?;
    let parsed = McpServersFile::parse(&raw)
        .with_context(|| format!("parsing {}", config_path.display()))?;
    let Some(server_config) = parsed.servers.get(server_id) else {
        bail!(
            "server `{server_id}` not found in {}; available: {:?}",
            config_path.display(),
            parsed.servers.keys().collect::<Vec<_>>()
        );
    };

    println!(
        "Importing MCP server `{server_id}` from {}",
        config_path.display()
    );
    let env_keys = server_config.env.keys().cloned().collect::<Vec<_>>();
    let secret_refs = server_config
        .secrets
        .iter()
        .map(|secret| secret.name.as_str())
        .collect::<Vec<_>>();
    let import_timeout_secs = server_config
        .sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.import_timeout_secs)
        .unwrap_or(30);
    println!(
        "MCP import config: transport=stdio command={} args={:?} env_keys={:?} secret_refs={:?} import_timeout={}s",
        server_config.command, server_config.args, env_keys, secret_refs, import_timeout_secs,
    );
    let importer = Importer::new(&EnvSecretResolver);
    importer
        .import(
            server_config,
            ImportOptions {
                server_id: server_id.to_string(),
                sandbox_profile: None,
            },
        )
        .await
        .with_context(|| format!("importing MCP server `{server_id}`"))
}

/// Print human-readable imported snapshot summary.
pub fn print_mcp_snapshot_summary(snapshot: &McpServerSnapshot) {
    println!();
    println!(
        "Server: {}\n  protocol_version: {}\n  server_config_digest: {}",
        snapshot.server_id, snapshot.protocol_version, snapshot.server_config_digest,
    );
    if let Some(info) = &snapshot.server_info {
        println!("  server_info: {}", info);
    }
    if !snapshot.secret_refs.is_empty() {
        println!(
            "  secret_refs: {}",
            snapshot
                .secret_refs
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("\nTools ({}):", snapshot.tools.len());
    for tool in &snapshot.tools {
        println!(
            "  - {}\n      mcp_name: {}\n      access: {}\n      schema_digest: {}\n      output_mode: {:?}{}",
            tool.platform_tool_name,
            tool.mcp_tool_name,
            tool.access_level,
            tool.input_schema_digest,
            tool.output_mode,
            tool.opaque_fallback_reason
                .as_deref()
                .map(|r| format!("\n      opaque_fallback: {r}"))
                .unwrap_or_default(),
        );
    }
    println!();
}

/// Mark server and all tools approved before registry import.
pub fn approve_mcp_snapshot(snapshot: &mut McpServerSnapshot) {
    use baml_rt_tools::mcp_snapshot::McpApprovalState;

    let owner = std::env::var("MCP_APPROVER_EMAIL")
        .ok()
        .or_else(|| std::env::var("GIT_AUTHOR_EMAIL").ok());
    let reviewed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("epoch:{}", d.as_secs()))
        .ok();
    let prior_state = snapshot.approval.state;
    snapshot.approval.state = McpApprovalState::Approved;
    tracing::info!(
        target: "mcp.approval",
        mcp_server_id = %snapshot.server_id,
        event = "mcp.approval_transition",
        from = ?prior_state,
        to = ?McpApprovalState::Approved,
        owner = ?owner,
        "MCP server approved",
    );
    snapshot.approval.owner = owner.clone();
    snapshot.approval.reviewed_at = reviewed_at.clone();
    for tool in &mut snapshot.tools {
        tool.approval.state = McpApprovalState::Approved;
        tool.approval.owner = owner.clone();
        tool.approval.reviewed_at = reviewed_at.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_runner_token_precedence() {
        let result =
            resolve_runner_token_from_sources(Some("flag"), Some("env".to_string())).unwrap();
        assert_eq!(result.as_deref(), Some("flag"));

        let result = resolve_runner_token_from_sources(None, Some("env-val".to_string())).unwrap();
        assert_eq!(result.as_deref(), Some("env-val"));

        let result = resolve_runner_token_from_sources(None, None).unwrap();
        assert!(result.is_none());

        let err = resolve_runner_token_from_sources(Some(""), None).unwrap_err();
        assert!(err.to_string().contains("empty or whitespace-only"));

        let err = resolve_runner_token_from_sources(Some("   "), None).unwrap_err();
        assert!(err.to_string().contains("empty or whitespace-only"));

        let err = resolve_runner_token_from_sources(None, Some("".to_string())).unwrap_err();
        assert!(err.to_string().contains("empty or whitespace-only"));

        let result = resolve_runner_token_from_sources(Some("  abc  "), None).unwrap();
        assert_eq!(result.as_deref(), Some("abc"));
    }
}
