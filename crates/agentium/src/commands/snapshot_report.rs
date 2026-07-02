// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Report contents of an explicit exported snapshot cache used by offline CI.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use baml_rt_tools::{external_tool_cache, mcp_cache};
use console::style;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct SnapshotReport {
    snapshot_cache: String,
    mcp: Vec<McpEntry>,
    external_tools: Vec<ExternalToolEntry>,
}

#[derive(Debug, Serialize)]
struct McpEntry {
    server_id: String,
    approval_state: String,
    tools_digest: String,
    tool_count: usize,
}

#[derive(Debug, Serialize)]
struct ExternalToolEntry {
    name: String,
    approval_state: String,
    snapshot_digest: String,
    schema_digest: String,
    runtime_digest: String,
}

pub fn run(snapshot_cache: &str, json_output: bool) -> Result<()> {
    let root = Path::new(snapshot_cache);
    let report = build_report(root)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("{} {}", style("Snapshot cache:").bold(), root.display());
    println!("{}", style("MCP snapshots").bold());
    if report.mcp.is_empty() {
        println!("  none");
    } else {
        for entry in &report.mcp {
            println!(
                "  {} state={} tools={} digest={}",
                style(&entry.server_id).cyan(),
                entry.approval_state,
                entry.tool_count,
                entry.tools_digest
            );
        }
    }

    println!("{}", style("External-tool snapshots").bold());
    if report.external_tools.is_empty() {
        println!("  none");
    } else {
        for entry in &report.external_tools {
            println!(
                "  {} state={} snapshot={} schema={} runtime={}",
                style(&entry.name).cyan(),
                entry.approval_state,
                entry.snapshot_digest,
                entry.schema_digest,
                entry.runtime_digest
            );
        }
    }
    Ok(())
}

fn build_report(root: &Path) -> Result<SnapshotReport> {
    let mcp_root = mcp_cache::resolve_cache_root(root);
    let external_root = external_tool_cache::resolve_cache_root(root);

    let mut mcp = Vec::new();
    let servers_dir = mcp_root.join("servers");
    if servers_dir.exists() {
        for entry in fs::read_dir(&servers_dir)
            .with_context(|| format!("reading {}", servers_dir.display()))?
        {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let server_id = entry.file_name().to_string_lossy().to_string();
            let snapshot = mcp_cache::read_snapshot(&mcp_root, &server_id)
                .with_context(|| format!("reading MCP snapshot {server_id}"))?;
            mcp.push(McpEntry {
                server_id,
                approval_state: format!("{:?}", snapshot.approval.state),
                tools_digest: snapshot.tools_digest.to_string(),
                tool_count: snapshot.tools.len(),
            });
        }
    }
    mcp.sort_by(|a, b| a.server_id.cmp(&b.server_id));

    let mut external_tools = external_tool_cache::read_approved_snapshots(&external_root)?
        .into_iter()
        .map(|snapshot| ExternalToolEntry {
            name: snapshot.tool.name,
            approval_state: format!("{:?}", snapshot.approval.state),
            snapshot_digest: snapshot.snapshot_digest.to_string(),
            schema_digest: snapshot.digests.schema_digest.to_string(),
            runtime_digest: snapshot.digests.runtime_digest.to_string(),
        })
        .collect::<Vec<_>>();
    external_tools.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(SnapshotReport {
        snapshot_cache: root.display().to_string(),
        mcp,
        external_tools,
    })
}
