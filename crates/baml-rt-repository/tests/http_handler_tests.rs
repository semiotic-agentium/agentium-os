//! HTTP handler tests using axum::test / tower::ServiceExt.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baml_rt_repository::{
    commands::{ForkCommand, PublishCommand, PublishOrigin},
    entry::{ChangeRationale, SourceBundle, Tag},
    lineage::EdgeDescription,
};
#[path = "support/common.rs"]
mod common;
use common::setup_app;
use tower::ServiceExt;

fn make_source(content: &str) -> SourceBundle {
    common::make_source(
        content,
        "http-test-agent",
        &["calculator"],
        "An agent for HTTP tests",
        &["compute"],
    )
}

async fn publish_agent(app: &axum::Router, name: &str, content: &str) -> serde_json::Value {
    let cmd = PublishCommand {
        name: name.parse().unwrap(),
        source: make_source(content),
        rationale: ChangeRationale::new("test publish").unwrap(),
        origin: PublishOrigin::Original,
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/publish")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK, "publish failed");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

// -------------------------------------------------------------------------
// Publish endpoint
// -------------------------------------------------------------------------

#[tokio::test]
async fn post_publish_returns_ok() {
    let app = setup_app().await;
    let result = publish_agent(&app, "http-agent", "export function run() {}");
    let body = result.await;
    assert!(body.get("hash").is_some());
    assert!(body.get("version_ref").is_some());
}

#[tokio::test]
async fn post_publish_duplicate_hash_returns_409() {
    let app = setup_app().await;
    let source = common::make_source(
        "same content",
        "dup-shared-manifest",
        &["calculator"],
        "shared",
        &["stable"],
    );
    let cmd1 = PublishCommand {
        name: "dup-hash-agent-a".parse().unwrap(),
        source: source.clone(),
        rationale: ChangeRationale::new("initial").unwrap(),
        origin: PublishOrigin::Original,
    };
    let cmd2 = PublishCommand {
        name: "dup-hash-agent-b".parse().unwrap(),
        source,
        rationale: ChangeRationale::new("initial").unwrap(),
        origin: PublishOrigin::Original,
    };

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/publish")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/publish")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd2).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
}

// -------------------------------------------------------------------------
// List agents endpoint
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_agents_returns_list() {
    let app = setup_app().await;
    publish_agent(&app, "agent-alpha", "alpha code").await;
    publish_agent(&app, "agent-beta", "beta code").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let agents = json["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
}

// -------------------------------------------------------------------------
// Get by hash
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_entry_by_hash() {
    let app = setup_app().await;
    let published = publish_agent(&app, "hash-agent", "hash test code").await;
    let hash = published["hash"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/entries/{hash}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_entry_by_hash_via_entries_route() {
    let app = setup_app().await;
    let published = publish_agent(&app, "hash-agent-alt", "hash test code").await;
    let hash = published["hash"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/entries/{hash}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_entry_by_hash_not_found() {
    let app = setup_app().await;
    let fake_hash = format!("{:0>64}", "deadbeef");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/entries/{fake_hash}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// -------------------------------------------------------------------------
// Get by version
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_entry_by_name_and_version() {
    let app = setup_app().await;
    publish_agent(&app, "version-agent", "version test code").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/entries/version-agent/v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// -------------------------------------------------------------------------
// List versions
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_agent_versions() {
    let app = setup_app().await;
    publish_agent(&app, "multi-ver-http", "v1 code").await;

    // Publish v2
    let cmd = PublishCommand {
        name: "multi-ver-http".parse().unwrap(),
        source: make_source("v2 code"),
        rationale: ChangeRationale::new("update").unwrap(),
        origin: PublishOrigin::Iteration,
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/publish")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/agents/multi-ver-http/versions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let versions = json["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
}

// -------------------------------------------------------------------------
// Entries list/query endpoint
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_entries_lists_all() {
    let app = setup_app().await;
    publish_agent(&app, "entries-a", "alpha").await;
    publish_agent(&app, "entries-b", "beta").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/entries")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["total"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn get_entries_by_name_and_version_query() {
    let app = setup_app().await;
    publish_agent(&app, "entries-query", "v1").await;
    let cmd = PublishCommand {
        name: "entries-query".parse().unwrap(),
        source: make_source("v2"),
        rationale: ChangeRationale::new("update").unwrap(),
        origin: PublishOrigin::Iteration,
    };
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/publish")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/entries?name=entries-query&version=v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["total"].as_u64().unwrap(), 1);
}

#[tokio::test]
async fn get_entries_query_version_without_name_is_400() {
    let app = setup_app().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/entries?version=v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// -------------------------------------------------------------------------
// Fork endpoint
// -------------------------------------------------------------------------

#[tokio::test]
async fn post_fork_returns_ok() {
    let app = setup_app().await;
    let published = publish_agent(&app, "fork-source", "original code").await;
    let source_hash: String = serde_json::from_value(published["hash"].clone()).unwrap();

    let cmd = ForkCommand {
        source_hash: source_hash.parse().unwrap(),
        new_name: "fork-target".parse().unwrap(),
        source: make_source("forked code"),
        rationale: ChangeRationale::new("forking for new purpose").unwrap(),
        fork_description: EdgeDescription::new("adapted for production").unwrap(),
        tags: vec![Tag::new("forked")],
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fork")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["generation"], 1);
}

// -------------------------------------------------------------------------
// Search endpoint
// -------------------------------------------------------------------------

#[tokio::test]
async fn post_search_returns_results() {
    let app = setup_app().await;
    publish_agent(&app, "search-http-agent", "searchable code").await;

    let query = serde_json::json!({
        "name": "search-http-agent"
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/search")
                .header("content-type", "application/json")
                .body(Body::from(query.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["total"], 1);
}

// -------------------------------------------------------------------------
// Lineage endpoint
// -------------------------------------------------------------------------

#[tokio::test]
async fn get_lineage_returns_subgraph() {
    let app = setup_app().await;
    let parent = publish_agent(&app, "lineage-parent", "parent code").await;
    let parent_hash: String = serde_json::from_value(parent["hash"].clone()).unwrap();

    // Fork to create lineage
    let cmd = ForkCommand {
        source_hash: parent_hash.parse().unwrap(),
        new_name: "lineage-child".parse().unwrap(),
        source: make_source("child code"),
        rationale: ChangeRationale::new("forking").unwrap(),
        fork_description: EdgeDescription::new("derived").unwrap(),
        tags: vec![],
    };
    let fork_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fork")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let fork_body = axum::body::to_bytes(fork_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let fork_json: serde_json::Value = serde_json::from_slice(&fork_body).unwrap();
    let child_hash = fork_json["hash"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/lineage/{child_hash}?depth=5"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!json["subgraph"]["ancestors"].as_array().unwrap().is_empty());
}

// -------------------------------------------------------------------------
// Tag endpoints
// -------------------------------------------------------------------------

#[tokio::test]
async fn add_and_remove_tag_via_http() {
    let app = setup_app().await;
    let published = publish_agent(&app, "tag-http-agent", "tag code").await;
    let hash = published["hash"].as_str().unwrap();

    // Add tag
    let add_body = serde_json::json!({ "tag": "production" });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/entries/{hash}/tags"))
                .header("content-type", "application/json")
                .body(Body::from(add_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Remove tag
    let remove_body = serde_json::json!({ "tag": "production" });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/entries/{hash}/tags"))
                .header("content-type", "application/json")
                .body(Body::from(remove_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// -------------------------------------------------------------------------
// Blob endpoints
// -------------------------------------------------------------------------

#[tokio::test]
async fn publish_does_not_store_blob_by_default() {
    let app = setup_app().await;
    let published = publish_agent(&app, "blob-http-agent", "export function run() {}").await;
    let hash = published["hash"].as_str().unwrap();

    let get_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/blobs/{}", hash))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_blob_not_found_returns_404() {
    let app = setup_app().await;
    let hash = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/blobs/{hash}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_blob_route_not_available() {
    let app = setup_app().await;
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/blobs/{hash}"))
                .header("content-type", "application/gzip")
                .body(Body::from(b"payload".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// -------------------------------------------------------------------------
// Error responses
// -------------------------------------------------------------------------

#[tokio::test]
async fn bad_hash_returns_400() {
    let app = setup_app().await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/entries/not-a-valid-hash")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn fork_missing_parent_returns_422() {
    let app = setup_app().await;
    let fake_hash = format!("{:0>64}", "abcdef");

    let cmd = ForkCommand {
        source_hash: fake_hash.parse().unwrap(),
        new_name: "orphan".parse().unwrap(),
        source: make_source("code"),
        rationale: ChangeRationale::new("test").unwrap(),
        fork_description: EdgeDescription::new("no parent").unwrap(),
        tags: vec![],
    };

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fork")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&cmd).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// -------------------------------------------------------------------------
// MCP registry endpoints
// -------------------------------------------------------------------------

fn approved_mcp_record() -> baml_rt_tools::mcp_snapshot::ApprovalRecord {
    baml_rt_tools::mcp_snapshot::ApprovalRecord {
        state: baml_rt_tools::mcp_snapshot::McpApprovalState::Approved,
        owner: Some("operator@example.com".into()),
        reviewed_at: Some("epoch:1".into()),
        expires_at: None,
    }
}

fn mcp_snapshot() -> baml_rt_tools::mcp_snapshot::McpServerSnapshot {
    use baml_rt_tools::{
        mcp_snapshot::{
            Digest, MCP_SNAPSHOT_SCHEMA_VERSION, McpImportedTool, McpOutputMode, McpServerSnapshot,
            McpTransportRef, compute_tools_digest,
        },
        tools::ToolAccess,
    };

    let tools = vec![McpImportedTool {
        platform_tool_name: "mcp/meteo/get_meteo".into(),
        mcp_tool_name: "get_meteo".into(),
        description: Some("Get weather".into()),
        input_schema: serde_json::json!({ "type": "object" }),
        input_schema_digest: Digest::new("sha256:input"),
        output_mode: McpOutputMode::ContentEnvelope,
        access_level: ToolAccess::Read,
        approval: approved_mcp_record(),
        opaque_fallback_reason: None,
        annotations: serde_json::Value::Null,
    }];

    McpServerSnapshot {
        schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
        server_id: "meteo".into(),
        transport: McpTransportRef::Stdio {
            command_ref: "meteo-mcp".into(),
            args: vec![],
        },
        protocol_version: "2025-06-18".into(),
        server_info: None,
        server_config_digest: Digest::new("sha256:server"),
        server_identity_digest: Digest::new("sha256:identity"),
        tools_digest: compute_tools_digest(&tools),
        secret_refs: vec![],
        approval: approved_mcp_record(),
        sandbox_profile: Some("restricted".into()),
        tools,
    }
}

async fn import_mcp_snapshot(app: &axum::Router) -> serde_json::Value {
    let body = baml_rt_repository::http::ImportMcpSnapshotRequest {
        snapshot: mcp_snapshot(),
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/snapshots/import")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn mcp_snapshot_import_and_read_routes_work() {
    let app = setup_app().await;
    let imported = import_mcp_snapshot(&app).await;
    assert_eq!(imported["version"]["server_id"], "meteo");
    assert_eq!(imported["version"]["version"], 1);

    let servers = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(servers.status(), StatusCode::OK);
    let body = axum::body::to_bytes(servers.into_body(), usize::MAX)
        .await
        .unwrap();
    let servers: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(servers["servers"].as_array().unwrap().len(), 1);

    let versions = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers/meteo/versions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(versions.status(), StatusCode::OK);
    let body = axum::body::to_bytes(versions.into_body(), usize::MAX)
        .await
        .unwrap();
    let versions: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(versions["versions"].as_array().unwrap().len(), 1);

    let snapshot = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers/meteo/versions/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.status(), StatusCode::OK);

    let latest = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers/meteo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(latest.status(), StatusCode::OK);
}

#[tokio::test]
async fn mcp_tool_lookup_and_mark_stale_routes_work() {
    let app = setup_app().await;
    import_mcp_snapshot(&app).await;

    let lookup = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/tools?platform_tool_name=mcp%2Fmeteo%2Fget_meteo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(lookup.status(), StatusCode::OK);
    let body = axum::body::to_bytes(lookup.into_body(), usize::MAX)
        .await
        .unwrap();
    let lookup: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(lookup["total"], 1);
    assert_eq!(lookup["tools"][0]["approval_state"], "approved");

    let stale = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/servers/meteo/versions/1/mark-stale")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::OK);

    let lookup = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/tools?platform_tool_name=mcp%2Fmeteo%2Fget_meteo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(lookup.into_body(), usize::MAX)
        .await
        .unwrap();
    let lookup: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(lookup["tools"][0]["approval_state"], "stale");
}
