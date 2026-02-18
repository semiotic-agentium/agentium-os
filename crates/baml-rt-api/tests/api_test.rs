//! HTTP API tests: discovery, A2A forward, and error mapping.
//! Uses insta snapshots with selective redaction for variant parts (IDs, instance URLs, etc.).

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use baml_rt_a2a::AgentRegistry;
use baml_rt_api::api_router;
use baml_rt_core::{AgentDiscoveryEntry, AgentRouteKey, BamlRtError, BusStream, Result};
use futures_util::stream;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

/// Snapshot-friendly response: status + body with variant parts redacted.
fn response_snapshot(status: StatusCode, body: &[u8]) -> Value {
    let body_value: Value = serde_json::from_slice(body)
        .unwrap_or(Value::String(String::from_utf8_lossy(body).into_owned()));
    serde_json::json!({
        "status": status.as_u16(),
        "body": redact_variant_parts(body_value),
    })
}

/// Redact variant parts of JSON (UUIDs, instance/type in problem bodies) for stable snapshots.
fn redact_variant_parts(v: Value) -> Value {
    use serde_json::Value as V;
    match v {
        V::String(s) => {
            if looks_like_uuid(&s) {
                return V::String("[uuid]".to_string());
            }
            V::String(s)
        }
        V::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let redacted = match k.as_str() {
                    "instance" => V::String("[instance]".to_string()),
                    "type_url" => V::String("[type_url]".to_string()),
                    "type" => match &val {
                        V::String(s) if s.starts_with("http://") || s.starts_with("https://") => {
                            V::String("[type_url]".to_string())
                        }
                        _ => redact_variant_parts(val),
                    },
                    _ => redact_variant_parts(val),
                };
                out.insert(k, redacted);
            }
            V::Object(out)
        }
        V::Array(arr) => V::Array(arr.into_iter().map(redact_variant_parts).collect()),
        other => other,
    }
}

fn looks_like_uuid(s: &str) -> bool {
    let s = s.trim_matches(|c| c == '{' || c == '}');
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_hexdigit()))
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
}

/// Mock registry for testing: fixed list and configurable A2A response.
struct MockRegistry {
    entries: Vec<AgentDiscoveryEntry>,
    handle_ok: Option<Vec<Value>>,
    handle_err_message: Option<String>,
}

impl MockRegistry {
    fn with_entries(entries: Vec<AgentDiscoveryEntry>) -> Self {
        Self {
            entries,
            handle_ok: None,
            handle_err_message: None,
        }
    }

    fn with_handle_ok(mut self, responses: Vec<Value>) -> Self {
        self.handle_ok = Some(responses);
        self
    }

    fn with_handle_err_not_found(mut self, message: String) -> Self {
        self.handle_err_message = Some(message);
        self
    }
}

#[async_trait]
impl AgentRegistry for MockRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }

    async fn handle_a2a_stream(
        &self,
        _key: &AgentRouteKey,
        _request: Value,
    ) -> Result<BusStream<Value>> {
        if let Some(ref ok) = self.handle_ok {
            return Ok(Box::pin(stream::iter(ok.clone())));
        }
        if let Some(ref msg) = self.handle_err_message {
            return Err(BamlRtError::InvalidArgument(msg.clone()));
        }
        Err(BamlRtError::InvalidArgument(
            "Agent pkg/inst not found".to_string(),
        ))
    }
}

fn discovery_entry(pkg: &str, inst: &str, name: &str, version: &str) -> AgentDiscoveryEntry {
    AgentDiscoveryEntry {
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        name: name.to_string(),
        version: version.to_string(),
    }
}

#[tokio::test]
async fn get_agents_returns_discovery_list() {
    let entries = vec![
        discovery_entry("pkg-a", "default", "Agent A", "0.1.0"),
        discovery_entry("pkg-b", "default", "Agent B", "0.2.0"),
    ];
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(entries));
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let snapshot = response_snapshot(status, &body);
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn get_agents_empty_list_returns_200() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let snapshot = response_snapshot(status, &body);
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn post_a2a_unknown_agent_returns_404() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "P", "0.1.0")])
            .with_handle_err_not_found("Agent other/default not found".to_string()),
    );
    let app = api_router(registry, None, None);

    let body = serde_json::json!({"jsonrpc":"2.0","method":"tasks.list","params":{},"id":1});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/other/default/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let snapshot = response_snapshot(status, &body);
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn post_a2a_malformed_body_returns_400() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/a2a")
                .header("content-type", "application/json")
                .body(Body::from("[]"))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let snapshot = response_snapshot(status, &body);
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn post_a2a_success_returns_200() {
    let responses = vec![serde_json::json!({"jsonrpc":"2.0","result":["t1"],"id":1})];
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "P", "0.1.0")])
            .with_handle_ok(responses.clone()),
    );
    let app = api_router(registry, None, None);

    let body = serde_json::json!({"jsonrpc":"2.0","method":"tasks.list","params":{},"id":1});
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/a2a")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let snapshot = response_snapshot(status, &resp_body);
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn get_openapi_json_returns_spec() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let snapshot = response_snapshot(status, &body);
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn post_a2a_sse_returns_event_stream() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_handle_ok(vec![serde_json::json!({
                "jsonrpc": "2.0",
                "result": { "tasks": [], "totalSize": 0, "pageSize": 50 },
                "id": null
            })]),
    );
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/a2a/sse")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("data:"),
        "SSE response should contain data: lines"
    );
    assert!(
        body_str.contains("totalSize"),
        "SSE data should contain JSON-RPC result"
    );
}
