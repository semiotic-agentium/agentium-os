//! End-to-end runtime tests: resolver → handler → session against the fake
//! MCP fixture binary.

use std::path::Path;

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_mcp::{
    composite::CompositeResolver,
    fixture::{FakeMcpConfig, FakeMcpTool},
    importer::EnvSecretResolver,
    resolver::McpResolver,
};
use baml_rt_tools::{
    ExternalToolResolver,
    mcp_cache::{read_server, read_snapshot, write_snapshot},
    mcp_config::{McpConfigError, McpServersFile},
    mcp_schema_normalize::normalize,
    mcp_snapshot::{
        ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
        McpOutputMode, McpServerSnapshot, McpTransportRef, SecretRef,
        compute_server_identity_digest, compute_tools_digest,
    },
    tool_fsm::{ToolSessionId, ToolStep},
    tools::{ToolAccess, ToolName, ToolSessionContext},
};
use serde_json::json;

const FAKE_BIN: &str = env!("CARGO_BIN_EXE_fake-mcp-stdio");

fn write_fixture(path: &Path, config: &FakeMcpConfig) {
    std::fs::write(path, serde_json::to_vec(config).unwrap()).unwrap();
}

fn approved_tool(name: &str, schema: serde_json::Value) -> McpImportedTool {
    let input_schema_digest = normalize(&schema).digest;
    McpImportedTool {
        platform_tool_name: format!("mcp/grafana/{name}"),
        mcp_tool_name: name.into(),
        description: Some(format!("{name} description")),
        input_schema: schema,
        input_schema_digest,
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

/// Identity digest matching what `fake-mcp-stdio` advertises on initialize:
/// `serverInfo.name = "grafana"` and `capabilities.tools.listChanged = true`.
fn fixture_identity_digest() -> Digest {
    compute_server_identity_digest(
        &json!({ "tools": { "listChanged": true } }),
        &json!({ "name": "grafana" }),
    )
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
        server_identity_digest: fixture_identity_digest(),
        tools_digest: compute_tools_digest(&tools),
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

fn patch_snapshot_config_digest(fixture_path: &Path, cache_root: &Path) {
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
    let config = servers.servers.get("grafana").unwrap();
    let Ok(mut snapshot) = read_snapshot(cache_root, "grafana") else {
        return;
    };
    snapshot.server_config_digest = baml_rt_tools::mcp_snapshot::compute_server_config_digest(
        "grafana",
        &snapshot.protocol_version,
        config,
        Some(&snapshot.tools_digest),
    );
    write_snapshot(cache_root, &snapshot).unwrap();
}

fn build_resolver_no_patch(
    fixture_path: &Path,
    cache_root: &Path,
) -> McpResolver<EnvSecretResolver> {
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

fn build_resolver(fixture_path: &Path, cache_root: &Path) -> McpResolver<EnvSecretResolver> {
    patch_snapshot_config_digest(fixture_path, cache_root);
    build_resolver_no_patch(fixture_path, cache_root)
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

#[test]
fn stdio_config_rejects_empty_command_but_http_config_allows_it() {
    let stdio_json = r#"{ "mcpServers": { "grafana": { "command": "" } } }"#;
    let stdio_err = McpServersFile::parse(stdio_json).expect_err("stdio command is required");
    assert!(matches!(stdio_err, McpConfigError::EmptyCommand { .. }));

    let http_json = r#"
    {
      "mcpServers": {
        "grafana": {
          "transport": {
            "kind": "streamable_http",
            "url": "https://mcp.grafana.example.com/mcp"
          }
        }
      }
    }
    "#;
    let http = McpServersFile::parse(http_json).expect("http transport does not use command");
    let server = http.servers.get("grafana").expect("server present");
    assert!(server.command.is_empty());
    assert!(server.transport.is_some());
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
        drift_changes_schema: false,
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
async fn list_changed_with_schema_drift_marks_stale_and_persists() {
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
    let mut fixture = sample_fixture();
    fixture.drift_mode = true;
    fixture.drift_changes_schema = true;
    write_fixture(&fixture_path, &fixture);

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");

    // First call succeeds; fake server then emits tools/list_changed.
    {
        let mut session = handler
            .open_session(session_context(&name), json!({}))
            .await
            .unwrap();
        session.send(json!({"q": "cpu"})).await.unwrap();
        let _ = session.read(json!({})).await.unwrap();
        session.finish().await.unwrap();
    }

    // Drift handler runs async; poll until either the connection flips
    // stale or the per-call timeout budget elapses.
    let mut stale_seen = false;
    for _ in 0..50 {
        let mut session = handler
            .open_session(session_context(&name), json!({}))
            .await
            .unwrap();
        match session.send(json!({})).await {
            Err(err) => {
                let display = format!("{err:?}");
                if display.contains("stale") {
                    stale_seen = true;
                    break;
                }
            }
            Ok(()) => {
                let _ = session.read(json!({})).await;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        stale_seen,
        "schema-changing list_changed must flip connection to stale"
    );

    // Disk persistence: server record's approval state should be Stale so
    // the runner refuses to bind on next startup.
    let record = read_server(cache.path(), "grafana").expect("server record");
    assert_eq!(record.approval.state, McpApprovalState::Stale);
}

#[tokio::test]
async fn list_changed_without_schema_drift_is_treated_as_spurious() {
    // Fake server emits list_changed but only the description changed.
    // The tools_digest covers (name, input_schema_digest) only, so the
    // observed digest matches the snapshot and the connection stays healthy.
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
    let mut fixture = sample_fixture();
    fixture.drift_mode = true;
    fixture.drift_changes_schema = false;
    write_fixture(&fixture_path, &fixture);

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");

    // Trigger the notification.
    {
        let mut session = handler
            .open_session(session_context(&name), json!({}))
            .await
            .unwrap();
        session.send(json!({"q": "cpu"})).await.unwrap();
        let _ = session.read(json!({})).await.unwrap();
        session.finish().await.unwrap();
    }

    // Give the async drift handler ample wall-clock time to run + complete
    // its out-of-band tools/list before we sample for stale.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Subsequent calls must still succeed; on-disk approval untouched.
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();
    session
        .send(json!({"q": "memory"}))
        .await
        .expect("spurious list_changed must not break the connection");
    let _ = session.read(json!({})).await.unwrap();
    session.finish().await.unwrap();

    let record = read_server(cache.path(), "grafana").expect("server record");
    assert_eq!(record.approval.state, McpApprovalState::Approved);
}

#[tokio::test]
async fn list_changed_is_spurious_for_opaque_fallback_schema() {
    // Regression: tools whose `inputSchema` triggers `opaque_fallback_reason`
    // (e.g. uses `$ref`) must still produce the same canonical digest at
    // import time and at drift-handler time. A no-op `tools/list_changed`
    // for such a tool must not flip the snapshot to stale.
    let cache = tempfile::tempdir().unwrap();
    let opaque_schema = json!({
        "type": "object",
        "properties": { "child": { "$ref": "#/definitions/Foo" } }
    });
    assert!(
        normalize(&opaque_schema).opaque_fallback_reason.is_some(),
        "test precondition: schema must trigger opaque fallback"
    );
    write_snapshot(
        cache.path(),
        &approved_snapshot(vec![approved_tool(
            "search_dashboards",
            opaque_schema.clone(),
        )]),
    )
    .unwrap();
    let fixture_path = cache.path().join("fixture.json");
    let mut fixture = sample_fixture();
    fixture.tools = vec![FakeMcpTool {
        name: "search_dashboards".into(),
        description: Some("search".into()),
        input_schema: opaque_schema,
        call_result: json!({
            "content": [{ "type": "text", "text": "ok" }],
            "isError": false
        }),
    }];
    fixture.drift_mode = true;
    fixture.drift_changes_schema = false;
    write_fixture(&fixture_path, &fixture);

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");

    {
        let mut session = handler
            .open_session(session_context(&name), json!({}))
            .await
            .unwrap();
        session.send(json!({})).await.unwrap();
        let _ = session.read(json!({})).await.unwrap();
        session.finish().await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();
    session
        .send(json!({}))
        .await
        .expect("no-op list_changed on opaque-fallback schema must not flip stale");
    let _ = session.read(json!({})).await.unwrap();
    session.finish().await.unwrap();

    let record = read_server(cache.path(), "grafana").expect("server record");
    assert_eq!(record.approval.state, McpApprovalState::Approved);
}

#[tokio::test]
async fn launch_config_digest_mismatch_fails_at_resolve_time() {
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

    let resolver = build_resolver_no_patch(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let err = match resolver.resolve(&name) {
        Err(err) => err,
        Ok(_) => panic!("config digest mismatch must fail closed"),
    };
    assert!(
        err.to_string().contains("launch config digest mismatch"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn startup_tools_digest_mismatch_fails_before_tool_call() {
    let cache = tempfile::tempdir().unwrap();
    write_snapshot(
        cache.path(),
        &approved_snapshot(vec![approved_tool(
            "search_dashboards",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )]),
    )
    .unwrap();
    let fixture_path = cache.path().join("fixture.json");
    let mut fixture = sample_fixture();
    fixture.tools = vec![FakeMcpTool {
        name: "search_dashboards".into(),
        description: Some("search".into()),
        input_schema: json!({"type": "object", "properties": {"q": {"type": "number"}}}),
        call_result: json!({
            "content": [{ "type": "text", "text": "should not execute" }],
            "isError": false
        }),
    }];
    write_fixture(&fixture_path, &fixture);

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();

    let err = session
        .send(json!({"q": "cpu"}))
        .await
        .expect_err("startup tools digest mismatch must fail first send");
    let display = format!("{err:?}");
    assert!(
        display.contains("tool surface digest mismatch"),
        "unexpected error: {display}"
    );
}

#[tokio::test]
async fn identity_mismatch_fails_closed_on_first_send() {
    // Snapshot is approved against the fake server's real advertised
    // identity, but we corrupt it before writing so the runtime's recompute
    // step finds a mismatch. No tool call should ever execute.
    let cache = tempfile::tempdir().unwrap();
    let mut snap = approved_snapshot(vec![approved_tool(
        "search_dashboards",
        json!({"type": "object"}),
    )]);
    snap.server_identity_digest =
        Digest::new("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    write_snapshot(cache.path(), &snap).unwrap();
    let fixture_path = cache.path().join("fixture.json");
    write_fixture(&fixture_path, &sample_fixture());

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();

    // First send triggers lazy spawn → initialize → identity verification.
    let err = session
        .send(json!({}))
        .await
        .expect_err("identity mismatch must fail first send");
    let display = format!("{err:?}");
    assert!(
        display.contains("identity digest mismatch"),
        "unexpected error: {display}"
    );
}

#[tokio::test]
async fn identity_mismatch_when_server_name_differs() {
    // Server's advertised name differs from what was approved at import:
    // approved snapshot expects `grafana`, fixture reports `clickup`.
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
    let mut fixture = sample_fixture();
    fixture.server_name = Some("clickup".into());
    write_fixture(&fixture_path, &fixture);

    let resolver = build_resolver(&fixture_path, cache.path());
    let name = ToolName::parse("mcp/grafana/search_dashboards").unwrap();
    let (_metadata, handler) = resolver.resolve(&name).unwrap().expect("resolved");
    let mut session = handler
        .open_session(session_context(&name), json!({}))
        .await
        .unwrap();

    let err = session
        .send(json!({}))
        .await
        .expect_err("server name change must fail identity check");
    let display = format!("{err:?}");
    assert!(
        display.contains("identity digest mismatch"),
        "unexpected error: {display}"
    );
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
