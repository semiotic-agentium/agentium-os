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
        tags: vec![],
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
                .uri(format!("/entries/hash/{hash}"))
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
                .uri(format!("/entries/hash/{fake_hash}"))
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
        tags: vec![],
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
async fn put_and_get_blob_roundtrip() {
    let app = setup_app().await;
    let hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let payload = b"fake-tar-gz-bytes";

    let put_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/blobs/{hash}"))
                .header("content-type", "application/gzip")
                .body(Body::from(payload.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put_resp.status(), StatusCode::CREATED);

    let get_resp = app
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
    assert_eq!(get_resp.status(), StatusCode::OK);
    let ct = get_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok());
    assert_eq!(ct, Some("application/gzip"));

    let body = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), payload);
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
                .uri("/entries/hash/not-a-valid-hash")
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
