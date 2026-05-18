//! Builder-level codegen tests for MCP-imported tools.
//!
//! Writes an approved snapshot into a temp cache, points the builder at it
//! via `BAML_MCP_CACHE_DIR`, then runs `render_baml_tool_interfaces` and
//! verifies the generated BAML mentions the projected types.

#![recursion_limit = "256"]

use std::sync::Mutex;

use baml_rt_builder::builder::baml_gen::render_baml_tool_interfaces;
use baml_rt_tools::{
    mcp_builder_catalog::BUILDER_MCP_CACHE_ENV,
    mcp_cache::write_snapshot,
    mcp_snapshot::{
        ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
        McpOutputMode, McpServerSnapshot, McpTransportRef, SecretRef,
    },
    tools::ToolAccess,
};
use serde_json::{Value, json};

/// Serializes env-mutating tests across this file so concurrent runs do not
/// race on `BAML_MCP_CACHE_DIR`.
static ENV_GUARD: Mutex<()> = Mutex::new(());

struct CacheEnvScope {
    previous: Option<String>,
}

impl CacheEnvScope {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var(BUILDER_MCP_CACHE_ENV).ok();
        // SAFETY: tests in this file serialize through ENV_GUARD before
        // touching the env, and the scope guard restores the prior value on
        // drop. No other code in the workspace reads BUILDER_MCP_CACHE_ENV.
        unsafe { std::env::set_var(BUILDER_MCP_CACHE_ENV, path) };
        Self { previous }
    }
}

impl Drop for CacheEnvScope {
    fn drop(&mut self) {
        // SAFETY: see set().
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(BUILDER_MCP_CACHE_ENV, value),
                None => std::env::remove_var(BUILDER_MCP_CACHE_ENV),
            }
        }
    }
}

fn approved_tool(name: &str, schema: Value, fallback: Option<&str>) -> McpImportedTool {
    McpImportedTool {
        platform_tool_name: format!("mcp/grafana/{name}"),
        mcp_tool_name: name.into(),
        description: Some(format!("{name} description")),
        input_schema: schema,
        input_schema_digest: Digest::new("sha256:input"),
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
        server_config_digest: Digest::new("sha256:server"),
        server_identity_digest: Digest::new("sha256:identity"),
        tools_digest: Digest::new("sha256:tools"),
        secret_refs: vec![SecretRef {
            name: "GRAFANA_TOKEN".into(),
            version: None,
        }],
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
    let _guard = ENV_GUARD.lock().unwrap();
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
    let _scope = CacheEnvScope::set(dir.path());

    let output =
        render_baml_tool_interfaces(&["mcp/grafana/search_dashboards".to_string()]).unwrap();

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
    let _guard = ENV_GUARD.lock().unwrap();
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
    let _scope = CacheEnvScope::set(dir.path());

    let output =
        render_baml_tool_interfaces(&["mcp/grafana/search_dashboards".to_string()]).unwrap();

    assert!(
        output.contains("OpaqueJson"),
        "expected OpaqueJson fallback in BAML output. Got: {output}"
    );
}

#[test]
fn pending_snapshot_is_invisible_to_builder() {
    let _guard = ENV_GUARD.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut snap = approved_snapshot(vec![approved_tool(
        "search_dashboards",
        json!({ "type": "object" }),
        None,
    )]);
    snap.tools[0].approval.state = McpApprovalState::Pending;
    write_snapshot(dir.path(), &snap).unwrap();
    let _scope = CacheEnvScope::set(dir.path());

    let err = render_baml_tool_interfaces(&["mcp/grafana/search_dashboards".to_string()])
        .expect_err("pending tool should not be resolvable");
    let msg = err.to_string();
    assert!(
        msg.contains("Tool metadata missing for"),
        "expected actionable missing-tool error, got: {msg}"
    );
}
