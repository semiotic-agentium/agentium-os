// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! MCP registry operator commands.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use baml_rt_builder::mcp_registry::{McpRegistryEnableOptions, enable_mcp_registry_server};
use console::style;
use serde_json::Value;

use super::utils::RunnerToken;

pub fn list(repository_url: &str, json: bool) -> Result<()> {
    let body = get_json(repository_url, "/mcp/servers")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{}", style("MCP servers:").bold());
    for server in body
        .get("servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let latest = server
            .get("latest_version")
            .and_then(Value::as_u64)
            .map(|v| format!("v{v}"))
            .unwrap_or_else(|| "<none>".into());
        println!(
            "  {}  latest={}  created_at={}",
            value_str(server, "server_id").unwrap_or("<server>"),
            latest,
            value_str(server, "created_at").unwrap_or("<unknown>"),
        );
    }
    Ok(())
}

pub fn enable(
    server_id: &str,
    config: Option<&str>,
    repository_url: &str,
    yes: bool,
    runner_token: Option<RunnerToken>,
) -> Result<()> {
    let config_path = config.map(PathBuf::from);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(enable_mcp_registry_server(McpRegistryEnableOptions {
        server_id,
        config_path: config_path.as_deref(),
        repository_url,
        skip_prompt: yes,
        runner_token: runner_token.as_ref().map(RunnerToken::as_str),
    }))
}

pub fn server(
    server_id: &str,
    version: Option<u32>,
    repository_url: &str,
    json: bool,
) -> Result<()> {
    let path = match version {
        Some(version) => format!("/mcp/servers/{server_id}/versions/{version}"),
        None => format!("/mcp/servers/{server_id}"),
    };
    let snapshot = get_json(repository_url, &path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    print_snapshot_summary(&snapshot);
    Ok(())
}

pub fn versions(server_id: &str, repository_url: &str, json: bool) -> Result<()> {
    let body = get_json(
        repository_url,
        &format!("/mcp/servers/{server_id}/versions"),
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "{} {}",
        style("MCP server versions:").bold(),
        style(server_id).cyan()
    );
    for version in body
        .get("versions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!(
            "  v{}  state={}  snapshot={}  tools={}",
            value_u64(version, "version").unwrap_or_default(),
            value_str(version, "approval_state").unwrap_or("<unknown>"),
            value_str(version, "snapshot_digest").unwrap_or("<missing>"),
            value_str(version, "tools_digest").unwrap_or("<missing>"),
        );
    }
    Ok(())
}

pub fn tool(platform_tool_name: &str, repository_url: &str, json: bool) -> Result<()> {
    let encoded = percent_encode(platform_tool_name);
    let body = get_json(
        repository_url,
        &format!("/mcp/tools?platform_tool_name={encoded}"),
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "{} {}",
        style("MCP tool registry entries:").bold(),
        style(platform_tool_name).cyan()
    );
    for tool in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!(
            "  {}@v{}  mcp_name={}  access={}  state={}  schema={}",
            value_str(tool, "server_id").unwrap_or("<server>"),
            value_u64(tool, "server_version").unwrap_or_default(),
            value_str(tool, "mcp_tool_name").unwrap_or("<tool>"),
            value_str(tool, "access_level").unwrap_or("<access>"),
            value_str(tool, "approval_state").unwrap_or("<state>"),
            value_str(tool, "input_schema_digest").unwrap_or("<digest>"),
        );
    }
    Ok(())
}

fn get_json(repository_url: &str, path: &str) -> Result<Value> {
    let url = format!("{}{}", repository_url.trim_end_matches('/'), path);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let response = reqwest::Client::new()
            .get(url.as_str())
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GET {url} failed ({status}): {body}");
        }
        serde_json::from_str(&body).with_context(|| format!("parsing response from {url}"))
    })
}

fn print_snapshot_summary(snapshot: &Value) {
    let server_id = value_str(snapshot, "server_id").unwrap_or("<unknown>");
    println!(
        "{} {}",
        style("MCP server:").bold(),
        style(server_id).cyan()
    );
    println!(
        "  protocol={}  server_config={}  identity={}  tools={}",
        value_str(snapshot, "protocol_version").unwrap_or("<unknown>"),
        value_str(snapshot, "server_config_digest").unwrap_or("<missing>"),
        value_str(snapshot, "server_identity_digest").unwrap_or("<missing>"),
        value_str(snapshot, "tools_digest").unwrap_or("<missing>"),
    );
    if let Some(profile) = value_str(snapshot, "sandbox_profile") {
        println!("  sandbox_profile={profile}");
    }
    if let Some(state) = snapshot
        .get("approval")
        .and_then(|v| v.get("state"))
        .and_then(Value::as_str)
    {
        println!("  approval_state={state}");
    }
    if let Some(secrets) = snapshot.get("secret_refs").and_then(Value::as_array)
        && !secrets.is_empty()
    {
        let names: Vec<&str> = secrets
            .iter()
            .filter_map(|s| s.get("name").and_then(Value::as_str))
            .collect();
        println!("  secret_refs={}", names.join(","));
    }
    println!("  tools:");
    for tool in snapshot
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!(
            "    - {}  mcp_name={}  access={}  state={}  schema={}",
            value_str(tool, "platform_tool_name").unwrap_or("<platform_tool>"),
            value_str(tool, "mcp_tool_name").unwrap_or("<mcp_tool>"),
            value_str(tool, "access_level").unwrap_or("<access>"),
            tool.get("approval")
                .and_then(|v| v.get("state"))
                .and_then(Value::as_str)
                .unwrap_or("<state>"),
            value_str(tool, "input_schema_digest").unwrap_or("<digest>"),
        );
    }
}

fn value_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn value_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[expect(dead_code, reason = "reserved helper for MCP path normalization")]
fn normalize_path(path: Option<&str>) -> Option<String> {
    path.map(|p| PathBuf::from(p).display().to_string())
}
