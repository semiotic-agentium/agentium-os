// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! On-disk layout for MCP server/tool snapshots.
//!
//! ```text
//! <root>/
//!   servers/<server_id>/server.json     <- transport, digest, secret refs, approval
//!   tools/<platform_slug>/tool-snapshot.json   <- per-tool snapshot
//! ```
//!
//! `<platform_slug>` is the platform tool name with `/` replaced by `__`.
//! Per-tool dirs allow granular cleanup of stale tool entries without touching
//! the rest of the server's imports.

use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::mcp_snapshot::{McpImportedTool, McpServerSnapshot};

const SERVERS_DIR: &str = "servers";
const TOOLS_DIR: &str = "tools";
const SERVER_FILE: &str = "server.json";
const TOOL_METADATA_FILE: &str = "tool-snapshot.json";

/// Persisted on-disk shape of a server entry. Mirrors `McpServerSnapshot`
/// minus the embedded `tools` list — tools are stored as separate per-tool
/// files so they can be removed individually.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerRecord {
    pub schema_version: u32,
    pub server_id: String,
    pub transport: crate::mcp_snapshot::McpTransportRef,
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<serde_json::Value>,
    pub server_config_digest: crate::mcp_snapshot::Digest,
    pub server_identity_digest: crate::mcp_snapshot::Digest,
    pub tools_digest: crate::mcp_snapshot::Digest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_refs: Vec<crate::mcp_snapshot::SecretRef>,
    pub approval: crate::mcp_snapshot::ApprovalRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
}

/// Per-tool file. Embeds a back-reference to the owning server id so a tool
/// file is self-describing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolRecord {
    pub schema_version: u32,
    pub server_id: String,
    #[serde(flatten)]
    pub tool: McpImportedTool,
}

/// Returns the directory where the per-server record lives.
pub fn server_dir(root: &Path, server_id: &str) -> PathBuf {
    root.join(SERVERS_DIR).join(server_id)
}

/// Returns the directory where the per-tool record for the given platform
/// tool name lives.
pub fn tool_dir(root: &Path, platform_tool_name: &str) -> PathBuf {
    root.join(TOOLS_DIR).join(platform_slug(platform_tool_name))
}

/// `mcp/grafana/search_dashboards` -> `mcp__grafana__search_dashboards`.
pub fn platform_slug(platform_tool_name: &str) -> String {
    platform_tool_name.replace('/', "__")
}

/// Split a server snapshot into per-server and per-tool records and write
/// them to the cache root. Existing files are overwritten.
pub fn write_snapshot(root: &Path, snapshot: &McpServerSnapshot) -> io::Result<()> {
    let server_dir = server_dir(root, &snapshot.server_id);
    fs::create_dir_all(&server_dir)?;
    let record = ServerRecord {
        schema_version: snapshot.schema_version,
        server_id: snapshot.server_id.clone(),
        transport: snapshot.transport.clone(),
        protocol_version: snapshot.protocol_version.clone(),
        server_info: snapshot.server_info.clone(),
        server_config_digest: snapshot.server_config_digest,
        server_identity_digest: snapshot.server_identity_digest,
        tools_digest: snapshot.tools_digest,
        secret_refs: snapshot.secret_refs.clone(),
        approval: snapshot.approval.clone(),
        sandbox_profile: snapshot.sandbox_profile.clone(),
    };
    // Atomic writes: a crash mid-import otherwise leaves a half-written
    // `server.json` next to a fully-written tool record, which the runtime
    // sees as a tool whose owning server fails to parse.
    write_json_atomic(&server_dir.join(SERVER_FILE), &record)?;

    for tool in &snapshot.tools {
        let dir = tool_dir(root, &tool.platform_tool_name);
        fs::create_dir_all(&dir)?;
        let entry = ToolRecord {
            schema_version: snapshot.schema_version,
            server_id: snapshot.server_id.clone(),
            tool: tool.clone(),
        };
        write_json_atomic(&dir.join(TOOL_METADATA_FILE), &entry)?;
    }
    Ok(())
}

/// Read one server + all its tools back into an `McpServerSnapshot`.
pub fn read_snapshot(root: &Path, server_id: &str) -> io::Result<McpServerSnapshot> {
    let server_path = server_dir(root, server_id).join(SERVER_FILE);
    let record: ServerRecord = read_json(&server_path)?;
    let tools = read_tools_for_server(root, server_id)?;
    Ok(McpServerSnapshot {
        schema_version: record.schema_version,
        server_id: record.server_id,
        transport: record.transport,
        protocol_version: record.protocol_version,
        server_info: record.server_info,
        server_config_digest: record.server_config_digest,
        server_identity_digest: record.server_identity_digest,
        tools_digest: record.tools_digest,
        secret_refs: record.secret_refs,
        approval: record.approval,
        sandbox_profile: record.sandbox_profile,
        tools,
    })
}

/// Read the standalone server record without scanning tool files.
pub fn read_server(root: &Path, server_id: &str) -> io::Result<ServerRecord> {
    read_json(&server_dir(root, server_id).join(SERVER_FILE))
}

/// Read every tool record under `<root>/tools/` whose `server_id` matches.
///
/// Tool slugs are derived from platform tool names (`mcp/<server>/<tool>` →
/// `mcp__<server>__<tool>`), so we filter dir names by prefix before parsing
/// JSON. A 1000-tool cache with M tools per server reads only M files
/// instead of N·M.
pub fn read_tools_for_server(root: &Path, server_id: &str) -> io::Result<Vec<McpImportedTool>> {
    let tools_root = root.join(TOOLS_DIR);
    let mut out = Vec::new();
    let entries = match fs::read_dir(&tools_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(out),
        Err(err) => return Err(err),
    };
    let slug_prefix = format!("mcp__{}__", server_id);
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.starts_with(&slug_prefix) {
            continue;
        }
        let path = entry.path().join(TOOL_METADATA_FILE);
        if !path.is_file() {
            continue;
        }
        let record: ToolRecord = read_json(&path)?;
        // Defensive: a colliding slug from another server would still parse;
        // confirm the embedded `server_id` matches before yielding the tool.
        if record.server_id == server_id {
            out.push(record.tool);
        }
    }
    out.sort_by(|a, b| a.platform_tool_name.cmp(&b.platform_tool_name));
    Ok(out)
}

/// Read a single per-tool record by platform tool name.
pub fn read_tool(root: &Path, platform_tool_name: &str) -> io::Result<ToolRecord> {
    read_json(&tool_dir(root, platform_tool_name).join(TOOL_METADATA_FILE))
}

/// Remove a single tool entry. Idempotent: missing dir is not an error.
pub fn remove_tool(root: &Path, platform_tool_name: &str) -> io::Result<()> {
    let dir = tool_dir(root, platform_tool_name);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Remove a server and every tool record that references it. Idempotent.
pub fn remove_server(root: &Path, server_id: &str) -> io::Result<()> {
    for tool in read_tools_for_server(root, server_id).unwrap_or_default() {
        remove_tool(root, &tool.platform_tool_name)?;
    }
    let dir = server_dir(root, server_id);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Write JSON atomically: serialize to a sibling `.tmp` file then rename in
/// place. Both initial snapshot writes and drift transitions go through this
/// so a crash mid-rewrite never leaves the snapshot in a half-stale state.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Flip the on-disk approval state of a server record to `Stale`, atomically.
/// Returns the previous state. Idempotent: a record already in `Stale` is left
/// in place and `Ok(Stale)` is returned.
pub fn mark_server_stale(
    root: &Path,
    server_id: &str,
) -> io::Result<crate::mcp_snapshot::McpApprovalState> {
    use crate::mcp_snapshot::McpApprovalState;
    let path = server_dir(root, server_id).join(SERVER_FILE);
    let mut record: ServerRecord = read_json(&path)?;
    let prev = record.approval.state;
    if prev != McpApprovalState::Stale {
        record.approval.state = McpApprovalState::Stale;
        write_json_atomic(&path, &record)?;
    }
    Ok(prev)
}

/// Flip the on-disk approval state of a tool record to `Stale`, atomically.
pub fn mark_tool_stale(
    root: &Path,
    platform_tool_name: &str,
) -> io::Result<crate::mcp_snapshot::McpApprovalState> {
    use crate::mcp_snapshot::McpApprovalState;
    let path = tool_dir(root, platform_tool_name).join(TOOL_METADATA_FILE);
    let mut record: ToolRecord = read_json(&path)?;
    let prev = record.tool.approval.state;
    if prev != McpApprovalState::Stale {
        record.tool.approval.state = McpApprovalState::Stale;
        write_json_atomic(&path, &record)?;
    }
    Ok(prev)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|err| io::Error::new(ErrorKind::InvalidData, err))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        mcp_snapshot::{
            ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
            McpOutputMode, McpTransportRef, SecretRef,
        },
        tools::ToolAccess,
    };

    fn tool(name: &str) -> McpImportedTool {
        McpImportedTool {
            platform_tool_name: format!("mcp/fake/{name}"),
            mcp_tool_name: name.into(),
            description: None,
            input_schema: json!({ "type": "object" }),
            input_schema_digest: Digest::new(
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            ),
            output_mode: McpOutputMode::ContentEnvelope,
            access_level: ToolAccess::Read,
            approval: ApprovalRecord {
                state: McpApprovalState::Approved,
                owner: None,
                reviewed_at: None,
                expires_at: None,
            },
            opaque_fallback_reason: None,
            annotations: serde_json::Value::Null,
        }
    }

    fn snapshot() -> McpServerSnapshot {
        McpServerSnapshot {
            schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
            server_id: "fake".into(),
            transport: McpTransportRef::Stdio {
                command_ref: "fake-mcp".into(),
                args: vec![],
            },
            protocol_version: "2025-06-18".into(),
            server_info: None,
            server_config_digest: Digest::new(
                "sha256:2222222222222222222222222222222222222222222222222222222222222222",
            ),
            server_identity_digest: Digest::new(
                "sha256:3333333333333333333333333333333333333333333333333333333333333333",
            ),
            tools_digest: Digest::new(
                "sha256:4444444444444444444444444444444444444444444444444444444444444444",
            ),
            secret_refs: vec![SecretRef::stdio_env("fake/token")],
            approval: ApprovalRecord {
                state: McpApprovalState::Approved,
                owner: Some("op@example.com".into()),
                reviewed_at: Some("2026-05-13T00:00:00Z".into()),
                expires_at: None,
            },
            sandbox_profile: Some("mcp-import-restricted".into()),
            tools: vec![tool("search"), tool("query")],
        }
    }

    #[test]
    fn round_trip_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = snapshot();
        snap.tools
            .sort_by(|a, b| a.platform_tool_name.cmp(&b.platform_tool_name));
        write_snapshot(dir.path(), &snap).unwrap();
        let read_back = read_snapshot(dir.path(), "fake").unwrap();
        assert_eq!(snap, read_back);
    }

    #[test]
    fn read_tools_for_server_ignores_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let mut snap = snapshot();
        write_snapshot(dir.path(), &snap).unwrap();

        snap.server_id = "other".into();
        snap.tools = vec![tool("alien")];
        snap.tools[0].platform_tool_name = "mcp/other/alien".into();
        write_snapshot(dir.path(), &snap).unwrap();

        let fake = read_tools_for_server(dir.path(), "fake").unwrap();
        let names: Vec<&str> = fake.iter().map(|t| t.platform_tool_name.as_str()).collect();
        assert_eq!(names, vec!["mcp/fake/query", "mcp/fake/search"]);
    }

    #[test]
    fn remove_tool_is_granular_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snapshot();
        write_snapshot(dir.path(), &snap).unwrap();

        remove_tool(dir.path(), "mcp/fake/search").unwrap();
        remove_tool(dir.path(), "mcp/fake/search").unwrap(); // idempotent

        let remaining = read_tools_for_server(dir.path(), "fake").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].platform_tool_name, "mcp/fake/query");
    }

    #[test]
    fn remove_server_clears_record_and_tools() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(dir.path(), &snapshot()).unwrap();
        remove_server(dir.path(), "fake").unwrap();
        remove_server(dir.path(), "fake").unwrap(); // idempotent
        assert!(read_server(dir.path(), "fake").is_err());
        assert!(
            read_tools_for_server(dir.path(), "fake")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn mark_server_stale_flips_state_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(dir.path(), &snapshot()).unwrap();
        let prev = mark_server_stale(dir.path(), "fake").unwrap();
        assert_eq!(prev, McpApprovalState::Approved);
        let stored = read_server(dir.path(), "fake").unwrap();
        assert_eq!(stored.approval.state, McpApprovalState::Stale);
        let prev2 = mark_server_stale(dir.path(), "fake").unwrap();
        assert_eq!(prev2, McpApprovalState::Stale);
    }

    #[test]
    fn mark_tool_stale_flips_only_that_tool() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(dir.path(), &snapshot()).unwrap();
        mark_tool_stale(dir.path(), "mcp/fake/search").unwrap();
        let search = read_tool(dir.path(), "mcp/fake/search").unwrap();
        let query = read_tool(dir.path(), "mcp/fake/query").unwrap();
        assert_eq!(search.tool.approval.state, McpApprovalState::Stale);
        assert_eq!(query.tool.approval.state, McpApprovalState::Approved);
    }

    #[test]
    fn platform_slug_replaces_slashes() {
        assert_eq!(
            platform_slug("mcp/grafana/search_dashboards"),
            "mcp__grafana__search_dashboards"
        );
    }
}
