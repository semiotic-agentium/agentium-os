//! End-to-end importer tests against the fake MCP fixture binary.

use std::collections::BTreeMap;

use baml_rt_mcp::{
    fixture::{FakeMcpConfig, FakeMcpTool},
    importer::{EnvSecretResolver, ImportError, ImportOptions, Importer, SecretResolver},
};
use baml_rt_tools::{
    mcp_cache::{read_snapshot, write_snapshot},
    mcp_config::{McpServerConfig, SandboxConfig, SecretDecl},
    mcp_snapshot::{McpApprovalState, McpTransportRef},
};
use serde_json::json;

const FAKE_BIN: &str = env!("CARGO_BIN_EXE_fake-mcp-stdio");

fn write_config(config: &FakeMcpConfig) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("temp config");
    serde_json::to_writer(&mut file, config).expect("serialize config");
    file.as_file_mut().sync_all().expect("sync");
    file
}

fn server_config_for(fixture_path: &std::path::Path) -> McpServerConfig {
    McpServerConfig {
        transport: None,
        command: FAKE_BIN.into(),
        args: vec![fixture_path.to_string_lossy().into_owned()],
        env: BTreeMap::new(),
        secrets: vec![],
        sandbox: Some(SandboxConfig {
            profile: Some("mcp-import-restricted-tier1".into()),
            import_timeout_secs: Some(10),
            runtime_call_timeout_secs: None,
        }),
        description: None,
    }
}

fn sample_fixture() -> FakeMcpConfig {
    FakeMcpConfig {
        server_name: Some("grafana".into()),
        tools: vec![
            FakeMcpTool {
                name: "search_dashboards".into(),
                description: Some("Search Grafana dashboards".into()),
                input_schema: json!({
                    "type": "object",
                    "properties": { "q": { "type": "string" } },
                    "required": ["q"]
                }),
                call_result: json!({"content": []}),
            },
            FakeMcpTool {
                name: "list_alerts".into(),
                description: Some("List active alerts".into()),
                input_schema: json!({ "type": "object", "properties": {} }),
                call_result: json!({"content": []}),
            },
        ],
        progress_mode: false,
        drift_mode: false,
        drift_changes_schema: false,
        malformed_response: false,
    }
}

#[tokio::test]
async fn importer_produces_pending_snapshot_with_two_tools() {
    let fixture = write_config(&sample_fixture());
    let config = server_config_for(fixture.path());
    let importer = Importer::new(&EnvSecretResolver);
    let snapshot = importer
        .import(
            &config,
            ImportOptions {
                server_id: "grafana".into(),
                sandbox_profile: None,
            },
        )
        .await
        .expect("import");

    assert_eq!(snapshot.server_id, "grafana");
    assert_eq!(snapshot.protocol_version, "2025-06-18");
    assert_eq!(snapshot.approval.state, McpApprovalState::Pending);
    let names: Vec<&str> = snapshot
        .tools
        .iter()
        .map(|t| t.platform_tool_name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["mcp/grafana/list_alerts", "mcp/grafana/search_dashboards"]
    );
    for tool in &snapshot.tools {
        assert_eq!(tool.approval.state, McpApprovalState::Pending);
        assert!(tool.opaque_fallback_reason.is_none());
        assert!(tool.input_schema_digest.to_string().starts_with("sha256:"));
    }
    assert!(matches!(snapshot.transport, McpTransportRef::Stdio { .. }));
    assert!(
        snapshot
            .server_config_digest
            .to_string()
            .starts_with("sha256:")
    );
}

#[tokio::test]
async fn unsupported_input_schema_falls_back_to_opaque() {
    let mut fixture = sample_fixture();
    fixture.tools[0].input_schema = json!({
        "type": "object",
        "properties": {
            "ref": { "$ref": "#/definitions/Foo" }
        }
    });
    let file = write_config(&fixture);
    let config = server_config_for(file.path());
    let importer = Importer::new(&EnvSecretResolver);
    let snapshot = importer
        .import(
            &config,
            ImportOptions {
                server_id: "grafana".into(),
                sandbox_profile: None,
            },
        )
        .await
        .expect("import");

    let search = snapshot
        .tools
        .iter()
        .find(|t| t.mcp_tool_name == "search_dashboards")
        .expect("search tool");
    let reason = search
        .opaque_fallback_reason
        .as_deref()
        .expect("fallback reason");
    assert!(reason.contains("$ref"));
}

#[tokio::test]
async fn empty_tools_list_is_an_error() {
    let mut fixture = sample_fixture();
    fixture.tools.clear();
    let file = write_config(&fixture);
    let config = server_config_for(file.path());
    let importer = Importer::new(&EnvSecretResolver);
    let err = importer
        .import(
            &config,
            ImportOptions {
                server_id: "empty".into(),
                sandbox_profile: None,
            },
        )
        .await
        .expect_err("empty tools should fail");
    assert!(matches!(err, ImportError::NoTools { .. }));
}

#[tokio::test]
async fn snapshot_round_trips_through_cache() {
    let fixture = write_config(&sample_fixture());
    let config = server_config_for(fixture.path());
    let importer = Importer::new(&EnvSecretResolver);
    let snapshot = importer
        .import(
            &config,
            ImportOptions {
                server_id: "grafana".into(),
                sandbox_profile: None,
            },
        )
        .await
        .expect("import");

    let cache = tempfile::tempdir().unwrap();
    write_snapshot(cache.path(), &snapshot).unwrap();
    let read_back = read_snapshot(cache.path(), "grafana").unwrap();
    assert_eq!(read_back, snapshot);
}

#[tokio::test]
async fn missing_secret_yields_actionable_error() {
    struct EmptyResolver;
    impl SecretResolver for EmptyResolver {
        fn resolve(&self, _name: &str) -> Option<String> {
            None
        }
    }

    let fixture = write_config(&sample_fixture());
    let mut config = server_config_for(fixture.path());
    config.secrets = vec![SecretDecl {
        name: "REQUIRED_TOKEN".into(),
        description: None,
        reason: None,
    }];
    let importer = Importer::new(&EmptyResolver);
    let err = importer
        .import(
            &config,
            ImportOptions {
                server_id: "x".into(),
                sandbox_profile: None,
            },
        )
        .await
        .expect_err("missing secret");
    assert!(matches!(err, ImportError::MissingSecret { .. }));
}
