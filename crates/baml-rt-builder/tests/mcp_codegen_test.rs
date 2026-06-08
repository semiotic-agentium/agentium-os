// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Builder-level codegen tests for MCP-imported tools.
//!
//! Writes an approved snapshot into a temp registry projection, passes that
//! root explicitly to codegen, and verifies generated BAML mentions projected types.

#![recursion_limit = "256"]

use baml_rt_builder::builder::baml_gen::render_baml_tool_interfaces_with_mcp_root;
use baml_rt_tools::{
    mcp_cache::write_snapshot,
    mcp_snapshot::{
        ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
        McpOutputMode, McpServerSnapshot, McpTransportRef, SecretRef,
    },
    tools::ToolAccess,
};
use serde_json::{Value, json};

fn approved_tool(name: &str, schema: Value, fallback: Option<&str>) -> McpImportedTool {
    McpImportedTool {
        platform_tool_name: format!("mcp/grafana/{name}"),
        mcp_tool_name: name.into(),
        description: Some(format!("{name} description")),
        input_schema: schema,
        input_schema_digest: Digest::new(
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        ),
        output_mode: McpOutputMode::ContentEnvelope,
        access_level: ToolAccess::Read,
        approval: ApprovalRecord {
            state: McpApprovalState::Approved,
            owner: Some("op@example.com".into()),
            reviewed_at: Some("epoch:1".into()),
            expires_at: None,
        },
        opaque_fallback_reason: fallback.map(str::to_string),
        annotations: Value::Null,
    }
}

fn approved_snapshot(tools: Vec<McpImportedTool>) -> McpServerSnapshot {
    McpServerSnapshot {
        schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
        server_id: "grafana".into(),
        transport: McpTransportRef::Stdio {
            command_ref: "fake".into(),
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
        secret_refs: vec![SecretRef::stdio_env("GRAFANA_TOKEN")],
        approval: ApprovalRecord {
            state: McpApprovalState::Approved,
            owner: Some("op@example.com".into()),
            reviewed_at: Some("epoch:1".into()),
            expires_at: None,
        },
        sandbox_profile: Some("mcp-import-restricted-tier1".into()),
        tools,
    }
}

#[test]
fn renders_typed_input_for_supported_schema() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(
        dir.path(),
        &approved_snapshot(vec![approved_tool(
            "search_dashboards",
            json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "required": ["q"]
            }),
            None,
        )]),
    )
    .unwrap();
    let output = render_baml_tool_interfaces_with_mcp_root(
        &["mcp/grafana/search_dashboards".to_string()],
        Some(dir.path()),
    )
    .unwrap();

    assert!(
        output.contains("McpGrafanaSearchDashboardsInput"),
        "expected typed input class in BAML output. Got: {output}"
    );
    assert!(
        output.contains("McpGrafanaSearchDashboardsOutput"),
        "expected output class in BAML output. Got: {output}"
    );
    assert!(
        output.contains("McpGrafanaSearchDashboardsOpenStep"),
        "expected session scaffolding in BAML output. Got: {output}"
    );
}

#[test]
fn renders_opaque_input_for_unsupported_schema() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(
        dir.path(),
        &approved_snapshot(vec![approved_tool(
            "search_dashboards",
            json!({
                "type": "object",
                "properties": { "ref": { "$ref": "#/definitions/Foo" } }
            }),
            Some("unsupported `$ref`"),
        )]),
    )
    .unwrap();
    let output = render_baml_tool_interfaces_with_mcp_root(
        &["mcp/grafana/search_dashboards".to_string()],
        Some(dir.path()),
    )
    .unwrap();

    assert!(
        output.contains("OpaqueJson"),
        "expected OpaqueJson fallback in BAML output. Got: {output}"
    );
}

#[test]
fn pending_snapshot_is_invisible_to_builder() {
    let dir = tempfile::tempdir().unwrap();
    let mut snap = approved_snapshot(vec![approved_tool(
        "search_dashboards",
        json!({ "type": "object" }),
        None,
    )]);
    snap.tools[0].approval.state = McpApprovalState::Pending;
    write_snapshot(dir.path(), &snap).unwrap();
    let err = render_baml_tool_interfaces_with_mcp_root(
        &["mcp/grafana/search_dashboards".to_string()],
        Some(dir.path()),
    )
    .expect_err("pending tool should not be resolvable");
    let msg = err.to_string();
    assert!(
        msg.contains("Tool metadata missing for"),
        "expected actionable missing-tool error, got: {msg}"
    );
}
