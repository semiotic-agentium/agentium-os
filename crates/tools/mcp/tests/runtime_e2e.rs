//! End-to-end runtime tests: resolver → handler → session against the fake
//! MCP fixture binary.

use std::path::Path;

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_tools::{
    ExternalToolResolver,
    mcp_cache::write_snapshot,
    mcp_snapshot::{
        ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
        McpOutputMode, McpServerSnapshot, McpTransportRef, SecretRef,
    },
    tool_fsm::{ToolSessionId, ToolStep},
    tools::{ToolAccess, ToolName, ToolSessionContext},
};
use baml_tools_mcp::{
    CompositeResolver,
    fixture::{FakeMcpConfig, FakeMcpTool},
    importer::EnvSecretResolver,
    resolver::McpResolver,
};
use serde_json::json;

const FAKE_BIN: &str = env!("CARGO_BIN_EXE_fake-mcp-stdio");

fn write_fixture(path: &Path, config: &FakeMcpConfig) {
    std::fs::write(path, serde_json::to_vec(config).unwrap()).unwrap();
}

fn approved_tool(name: &str, schema: serde_json::Value) -> McpImportedTool {
    McpImportedTool {
        platform_tool_name: format!("mcp/grafana/{name}"),
        mcp_tool_name: name.into(),
        description: Some(format!("{name} description")),
        input_schema: schema,
        input_schema_digest: Digest::new("sha256:input"),
        prompt_digest: None,
        output_mode: McpOutputMode::ContentEnvelope,
        access_level: ToolAccess::Read,
        approval: ApprovalRecord {
            state: McpApprovalState::Approved,
            owner: Some("op@example.com".into()),
            reviewed_at: Some("epoch:1".into()),
            expires_at: None,
        },
        opaque_fallback_reason: None,
        annotations: serde_json::Value::Null,
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
        runtime_artifact_digest: None,
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

fn build_resolver(fixture_path: &Path, cache_root: &Path) -> McpResolver<EnvSecretResolver> {
    let json = serde_json::json!({
        "mcpServers": {
            "grafana": {
                "command": FAKE_BIN,
                "args": [fixture_path.to_string_lossy()],
                "env": {},
                "secrets": []
            }
        }
    });
    let servers = baml_rt_tools::mcp_config::McpServersFile::parse(&json.to_string()).unwrap();
    McpResolver::new(cache_root.to_path_buf(), servers, EnvSecretResolver)
}

fn session_context(name: &ToolName) -> ToolSessionContext {
    ToolSessionContext {
        session_id: ToolSessionId::random(),
        tool_name: name.clone(),
        context_id: ContextId::new(1, 1),
        agent_id: AgentId::from_uuid(
            UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
        ),
        config: None,
        config_version: None,
        task_id: None,
        execution_classifier: None,
    }
}

fn sample_fixture() -> FakeMcpConfig {
    FakeMcpConfig {
        server_name: Some("grafana".into()),
        tools: vec![
            FakeMcpTool {
                name: "search_dashboards".into(),
                description: Some("search".into()),
                input_schema: json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
                call_result: json!({
                    "content": [{ "type": "text", "text": "found 3 dashboards" }],
                    "isError": false
                }),
            },
            FakeMcpTool {
                name: "list_alerts".into(),
                description: Some("alerts".into()),
                input_schema: json!({ "type": "object", "properties": {} }),
                call_result: json!({
                    "content": [{ "type": "text", "text": "no alerts" }],
                    "isError": false
                }),
            },
        ],
        progress_mode: false,
        drift_mode: false,
        malformed_response: false,
    }
}

#[tokio::test]
async fn handler_round_trips_through_fake_server() {
    let cache = tempfile::tempdir().unwrap();
    write_snapshot(
        cache.path(),
        &approved_snapshot(vec![
            approved_tool(
                "search_dashboards",
                json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            ),
            approved_tool("list_alerts", json!({"type": "object", "properties": {}})),
        ]),
    )
    .unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (metadata, handler) = resolver.resolve(&name).unwrap().expect("tool resolves");
    assert_eq!(metadata.name, name);

    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();
    session.send(json!({ "q": "cpu" })).await.unwrap();
    let step = session.read(json!({})).await.unwrap();

    match step {
        ToolStep::Done {
            output: Some(envelope),
        } => {
            assert_eq!(envelope["is_error"], false);
            let first = envelope["content"]
                .as_array()
                .expect("content array")
                .first()
                .expect("first block");
            assert_eq!(first["text"], "found 3 dashboards");
        }
        other => panic!("expected Done with envelope, got {other:?}"),
    }

    session.finish().await.unwrap();
}

#[tokio::test]
async fn pending_tool_is_rejected_at_resolve_time() {
    let cache = tempfile::tempdir().unwrap();
    let mut snap = approved_snapshot(vec![approved_tool(
        "search_dashboards",
        json!({"type": "object"}),
    )]);
    snap.tools[0].approval.state = McpApprovalState::Pending;
    write_snapshot(cache.path(), &snap).unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    match resolver.resolve(&name) {
        Err(err) => assert!(
            err.to_string().contains("not approved"),
            "unexpected error: {err}"
        ),
        Ok(_) => panic!("expected pending tool to fail resolve"),
    }
}

#[tokio::test]
async fn unknown_tool_returns_none() {
    let cache = tempfile::tempdir().unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());
    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/missing").unwrap();
    let result = resolver.resolve(&name).unwrap();
    assert!(result.is_none(), "missing snapshot must not resolve");
}

#[tokio::test]
async fn non_mcp_name_is_ignored_by_resolver() {
    let cache = tempfile::tempdir().unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());
    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("support/calculator").unwrap();
    let result = resolver.resolve(&name).unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn composite_combines_mcp_and_inert_resolvers() {
    let cache = tempfile::tempdir().unwrap();
    write_snapshot(
        cache.path(),
        &approved_snapshot(vec![approved_tool(
            "search_dashboards",
            json!({"type": "object"}),
        )]),
    )
    .unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());

    let composite =
        CompositeResolver::new().with(Box::new(build_resolver(&fixture_path, cache.path())));
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    assert!(composite.resolve(&name).unwrap().is_some());
}

#[tokio::test]
async fn aborted_session_refuses_further_reads() {
    let cache = tempfile::tempdir().unwrap();
    write_snapshot(
        cache.path(),
        &approved_snapshot(vec![approved_tool(
            "search_dashboards",
            json!({"type": "object"}),
        )]),
    )
    .unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();
    session.abort(Some("client cancel".into())).await.unwrap();
    let read_err = session.read(json!({})).await;
    assert!(read_err.is_err());
}
