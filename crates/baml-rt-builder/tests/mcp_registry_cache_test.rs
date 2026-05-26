//! Builder/registry integration for MCP snapshot approval at the package boundary.
//!
//! Verifies that stale registry snapshots cannot be materialized into `build_dir/mcp/`
//! (and therefore cannot reach the agent tarball), while approved snapshots can.

use std::{fs, sync::Arc};

use baml_rt_builder::builder::{
    AgentDir, BuildDir, RuntimeTypeGenerator, StdPackager, TypeGenerator, traits::Packager,
};
use baml_rt_repository::{
    RepositoryService,
    storage::{BlobStore, LineageStore, McpRegistryStore, MetadataStore, SearchStore},
    surreal_store::SurrealStore,
};
use baml_rt_tools::{
    mcp_cache::{read_server, write_snapshot},
    mcp_snapshot::{
        ApprovalRecord, Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalState, McpImportedTool,
        McpOutputMode, McpServerSnapshot, McpTransportRef, SecretRef,
    },
    tools::ToolAccess,
};
use serde_json::json;

fn repository_service(store: Arc<SurrealStore>) -> Arc<RepositoryService> {
    Arc::new(RepositoryService::new(
        store.clone() as Arc<dyn BlobStore>,
        store.clone() as Arc<dyn MetadataStore>,
        store.clone() as Arc<dyn LineageStore>,
        store.clone() as Arc<dyn SearchStore>,
        store as Arc<dyn McpRegistryStore>,
    ))
}

fn approved_tool(name: &str) -> McpImportedTool {
    McpImportedTool {
        platform_tool_name: format!("mcp/grafana/{name}"),
        mcp_tool_name: name.into(),
        description: Some(format!("{name} description")),
        input_schema: json!({
            "type": "object",
            "properties": { "q": { "type": "string" } }
        }),
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
        opaque_fallback_reason: None,
        annotations: json!(null),
    }
}

fn grafana_snapshot() -> McpServerSnapshot {
    let tools = vec![approved_tool("search_dashboards")];
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

fn minimal_mcp_agent_dir(tools: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp agent dir");
    fs::create_dir_all(dir.path().join("baml_src")).expect("baml_src");
    fs::write(
        dir.path().join("manifest.json"),
        serde_json::json!({
            "version": "1.0.0",
            "name": "mcp-test-agent",
            "entry_point": "src/index.ts",
            "tools": tools,
        })
        .to_string(),
    )
    .expect("manifest");
    dir
}

#[tokio::test]
async fn stale_registry_snapshot_blocks_build_cache_materialization() {
    let store = Arc::new(SurrealStore::open_in_memory().await.expect("store"));
    let service = repository_service(store.clone());
    service
        .put_mcp_snapshot(&grafana_snapshot())
        .await
        .expect("insert snapshot");
    service
        .mark_mcp_version_stale("grafana", 1)
        .await
        .expect("mark stale");

    assert!(
        service
            .get_latest_mcp_snapshot("grafana")
            .await
            .expect("lookup")
            .is_none(),
        "latest approved lookup must hide stale registry versions"
    );

    let agent_dir = minimal_mcp_agent_dir(&["mcp/grafana/search_dashboards"]);
    let build_dir = BuildDir::new().expect("build dir");
    let generator = RuntimeTypeGenerator::with_mcp_registry_service(service);

    let err = generator
        .generate(
            &AgentDir::new(agent_dir.path().to_path_buf()).expect("agent dir"),
            &build_dir,
        )
        .await
        .expect_err("stale registry snapshot must fail before cache materialization");

    let msg = err.to_string();
    assert!(
        msg.contains("stale") || msg.contains("no approved registry snapshot"),
        "expected actionable stale-registry error, got: {msg}"
    );
    assert!(
        !build_dir.join("mcp").exists(),
        "stale registry snapshot must not be written into build_dir/mcp"
    );
}

#[tokio::test]
async fn approved_registry_snapshot_is_materialized_into_package_tarball() {
    let store = Arc::new(SurrealStore::open_in_memory().await.expect("store"));
    let service = repository_service(store);
    service
        .put_mcp_snapshot(&grafana_snapshot())
        .await
        .expect("insert snapshot");

    let latest = service
        .get_latest_mcp_snapshot("grafana")
        .await
        .expect("lookup")
        .expect("approved snapshot present");
    assert_eq!(latest.approval.state, McpApprovalState::Approved);

    let agent_dir = minimal_mcp_agent_dir(&["mcp/grafana/search_dashboards"]);
    let build_dir = BuildDir::new().expect("build dir");
    let mcp_root = build_dir.join("mcp");
    write_snapshot(&mcp_root, &latest).expect("materialize build cache");

    fs::create_dir_all(build_dir.join("baml_src")).expect("build baml_src");
    fs::write(
        build_dir.join("baml_src").join("stub.baml"),
        "class Dummy { x string }\n",
    )
    .expect("stub baml");

    let output = tempfile::NamedTempFile::new().expect("output tar");
    let packager = StdPackager::new();
    packager
        .package(
            &AgentDir::new(agent_dir.path().to_path_buf()).expect("agent dir"),
            &build_dir,
            output.path(),
        )
        .await
        .expect("package");

    let extract_dir = tempfile::tempdir().expect("extract dir");
    let tar_gz = fs::File::open(output.path()).expect("open tar");
    let tar = flate2::read::GzDecoder::new(tar_gz);
    tar::Archive::new(tar)
        .unpack(extract_dir.path())
        .expect("unpack");

    let packaged_server = extract_dir.path().join("mcp/servers/grafana/server.json");
    assert!(
        packaged_server.exists(),
        "approved MCP snapshot must be carried in the agent package under mcp/"
    );

    let record = read_server(&extract_dir.path().join("mcp"), "grafana").expect("read server");
    assert_eq!(record.approval.state, McpApprovalState::Approved);
}
