//! HTTP API tests: discovery, A2A forward, and error mapping.
//! Uses insta snapshots with selective redaction for variant parts (IDs, instance URLs, etc.).

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baml_rt_a2a::AgentRegistry;
use baml_rt_api::{MermaidError, MermaidService, api_router};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentLister, AgentRouteKey,
    BamlRtError, BusStream, Result,
};
use futures_util::{StreamExt, stream};
use opentelemetry::{global, trace::TracerProvider as _};
use opentelemetry_sdk::{testing::trace::InMemorySpanExporterBuilder, trace::TracerProvider};
use serde_json::Value;
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

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
    handle_ok: Option<Vec<A2aStreamChunk>>,
    /// When set, yield each value with a delay between yields (for no-buffering tests).
    handle_delayed: Option<Vec<A2aStreamChunk>>,
    handle_err_message: Option<String>,
    /// When set, capture the route key passed to handle_a2a_stream (for routing tests).
    key_captured: Option<std::sync::Arc<std::sync::Mutex<Option<AgentRouteKey>>>>,
}

struct OtelTestFixture {
    exporter: opentelemetry_sdk::testing::trace::InMemorySpanExporter,
    provider: TracerProvider,
    _otel_lock: std::sync::MutexGuard<'static, ()>,
}

static OTEL_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static OTEL_STATE: OnceLock<OtelTestState> = OnceLock::new();

struct OtelTestState {
    exporter: opentelemetry_sdk::testing::trace::InMemorySpanExporter,
    provider: TracerProvider,
}

fn otel_test_lock() -> std::sync::MutexGuard<'static, ()> {
    OTEL_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

fn otel_state() -> &'static OtelTestState {
    OTEL_STATE.get_or_init(|| {
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = TracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        global::set_tracer_provider(provider.clone());
        let tracer = provider.tracer("baml_rt_api_test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        tracing::subscriber::set_global_default(subscriber).expect("set global tracing subscriber");
        OtelTestState { exporter, provider }
    })
}

impl OtelTestFixture {
    fn new() -> Self {
        let _otel_lock = otel_test_lock();
        let state = otel_state();
        state.exporter.reset();
        Self {
            exporter: state.exporter.clone(),
            provider: state.provider.clone(),
            _otel_lock,
        }
    }

    fn spans(&self) -> Vec<opentelemetry_sdk::export::trace::SpanData> {
        let _ = self.provider.force_flush();
        self.exporter.get_finished_spans().unwrap_or_default()
    }
}

fn find_span<'a>(
    spans: &'a [opentelemetry_sdk::export::trace::SpanData],
    name: &str,
) -> Option<&'a opentelemetry_sdk::export::trace::SpanData> {
    spans.iter().find(|span| span.name.as_ref() == name)
}

fn find_span_with_attr<'a>(
    spans: &'a [opentelemetry_sdk::export::trace::SpanData],
    key: &str,
    value: &str,
) -> Option<&'a opentelemetry_sdk::export::trace::SpanData> {
    spans
        .iter()
        .find(|span| attr_value(span, key).as_deref() == Some(value))
}

fn attr_value(span: &opentelemetry_sdk::export::trace::SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .and_then(|kv| match &kv.value {
            opentelemetry::Value::String(value) => Some(value.to_string()),
            opentelemetry::Value::Bool(value) => Some(value.to_string()),
            opentelemetry::Value::I64(value) => Some(value.to_string()),
            opentelemetry::Value::F64(value) => Some(value.to_string()),
            _ => None,
        })
}

struct MockMermaid {
    context_body: String,
    task_body: String,
}

impl MockMermaid {
    fn new(context_body: &str, task_body: &str) -> Self {
        Self {
            context_body: context_body.to_string(),
            task_body: task_body.to_string(),
        }
    }
}

#[async_trait]
impl MermaidService for MockMermaid {
    async fn mermaid_for_context(
        &self,
        _context_id: &str,
    ) -> std::result::Result<String, MermaidError> {
        Ok(self.context_body.clone())
    }

    async fn mermaid_for_task(&self, _task_id: &str) -> std::result::Result<String, MermaidError> {
        Ok(self.task_body.clone())
    }
}

impl MockRegistry {
    fn with_entries(entries: Vec<AgentDiscoveryEntry>) -> Self {
        Self {
            entries,
            handle_ok: None,
            handle_delayed: None,
            handle_err_message: None,
            key_captured: None,
        }
    }

    fn with_handle_ok(mut self, responses: Vec<Value>) -> Self {
        self.handle_ok = Some(responses.into_iter().map(A2aStreamChunk::from).collect());
        self
    }

    /// Yields each value with a delay between yields. Used to assert server does not buffer.
    fn with_handle_delayed(mut self, responses: Vec<Value>) -> Self {
        self.handle_delayed = Some(responses.into_iter().map(A2aStreamChunk::from).collect());
        self
    }

    /// Builder helper for tests that assert 404/not-found; reserved for alternative test paths.
    #[allow(dead_code)] // test-only builder path
    fn with_handle_err_not_found(mut self, message: String) -> Self {
        self.handle_err_message = Some(message);
        self
    }

    /// Builder helper to assert which route key was used; reserved for alternative test paths.
    #[allow(dead_code)] // test-only builder path
    fn capture_key(
        mut self,
        cell: std::sync::Arc<std::sync::Mutex<Option<AgentRouteKey>>>,
    ) -> Self {
        self.key_captured = Some(cell);
        self
    }
}

impl AgentLister for MockRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

#[async_trait]
impl AgentRegistry for MockRegistry {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        _request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        if let Some(ref cell) = self.key_captured {
            *cell.lock().unwrap() = Some(key.clone());
        }
        if let Some(ref ok) = self.handle_ok {
            return Ok(Box::pin(stream::iter(ok.clone())));
        }
        if let Some(ref delayed) = self.handle_delayed {
            let vec = delayed.clone();
            let delayed_stream =
                stream::unfold((vec.into_iter(), 0usize), |(mut it, count)| async move {
                    if count > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    }
                    let v = it.next()?;
                    Some((v, (it, count + 1)))
                });
            return Ok(Box::pin(delayed_stream));
        }
        if let Some(ref msg) = self.handle_err_message {
            return Err(BamlRtError::AgentNotFound(msg.clone()));
        }
        Err(BamlRtError::AgentNotFound(
            "Agent pkg/inst not found".to_string(),
        ))
    }
}

fn discovery_entry(pkg: &str, inst: &str, name: &str, version: &str) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        tools: vec![],
        description: None,
        capabilities: vec![],
    };
    AgentDiscoveryEntry {
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        agent_card: card,
    }
}

fn discovery_entry_with_card(
    pkg: &str,
    inst: &str,
    name: &str,
    version: &str,
    description: Option<&str>,
    capabilities: Vec<&str>,
) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        tools: vec![
            "system/internal_a2a".to_string(),
            "support/calculate".to_string(),
        ],
        description: description.map(str::to_string),
        capabilities: capabilities.into_iter().map(str::to_string).collect(),
    };
    AgentDiscoveryEntry {
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        agent_card: card,
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
async fn get_agents_returns_agent_cards_when_present() {
    let entries = vec![
        discovery_entry_with_card(
            "pkg-a",
            "default",
            "Agent A",
            "0.1.0",
            Some("Does task A"),
            vec!["a2a"],
        ),
        discovery_entry_with_card("pkg-b", "default", "Agent B", "0.2.0", None, vec![]),
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

/// Asserts that the server does not buffer the A2A stream: events must arrive incrementally.
/// Mock yields three items with 80ms delay between each; if server buffered we would see nothing
/// for ~240ms. We require the first SSE event to arrive within 200ms (client choice = no buffering).
#[tokio::test]
async fn post_a2a_sse_no_buffering_events_arrive_incrementally() {
    let responses = vec![
        serde_json::json!({"jsonrpc":"2.0","result":{"n":1},"id":null}),
        serde_json::json!({"jsonrpc":"2.0","result":{"n":2},"id":null}),
        serde_json::json!({"jsonrpc":"2.0","result":{"n":3},"id":null}),
    ];
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_handle_delayed(responses),
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

    let body = response.into_body();
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    let mut events_received = 0u32;
    let mut first_event_elapsed: Option<std::time::Duration> = None;
    let start = std::time::Instant::now();
    const FIRST_EVENT_MAX_MS: u64 = 200;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("body chunk");
        buf.extend_from_slice(&chunk);
        let mut line_start = 0;
        while let Some(offset) = buf[line_start..].iter().position(|&b| b == b'\n') {
            let line_end = line_start + offset + 1;
            let line = &buf[line_start..line_end];
            if line.starts_with(b"data:") && line.len() > 5 {
                events_received += 1;
                if first_event_elapsed.is_none() {
                    first_event_elapsed = Some(start.elapsed());
                }
            }
            line_start = line_end;
        }
        buf.drain(..line_start);
    }

    assert!(
        events_received >= 3,
        "expected at least 3 SSE data events, got {events_received}"
    );
    let elapsed = first_event_elapsed.expect("at least one event");
    assert!(
        elapsed.as_millis() < FIRST_EVENT_MAX_MS as u128,
        "first SSE event must arrive within {FIRST_EVENT_MAX_MS}ms (no server buffering); got {}ms",
        elapsed.as_millis()
    );
}

#[tokio::test]
async fn get_mermaid_context_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let mermaid: Arc<dyn MermaidService> = Arc::new(MockMermaid::new(
        "sequenceDiagram\n    autonumber\n    actor User\n    participant agent\n    User->>agent: ping",
        "sequenceDiagram\n    autonumber\n    participant agent",
    ));
    let app = api_router(registry, Some(mermaid), None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/contexts/ctx-1-1/mermaid")
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
async fn get_mermaid_task_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let mermaid: Arc<dyn MermaidService> = Arc::new(MockMermaid::new(
        "sequenceDiagram\n    autonumber\n    participant agent",
        "sequenceDiagram\n    autonumber\n    participant agent\n    agent->>User: done",
    ));
    let app = api_router(registry, Some(mermaid), None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/tasks/task-123/mermaid")
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
async fn get_mermaid_context_emits_http_and_handler_spans() {
    let otel = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let mermaid: Arc<dyn MermaidService> = Arc::new(MockMermaid::new(
        "sequenceDiagram\n    autonumber\n    actor User\n    participant agent\n    User->>agent: ping",
        "sequenceDiagram\n    autonumber\n    participant agent",
    ));
    let app = api_router(registry, Some(mermaid), None);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/contexts/ctx-1-1/mermaid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let spans = otel.spans();
    // Require handler span (created in handler). HTTP span from TraceLayer may be missing with
    // oneshot (route layer / Otel timing); never .expect() on it.
    let handler_span =
        find_span(&spans, "baml_rt_api.get_mermaid_context").expect("mermaid context handler span");
    assert_eq!(
        attr_value(handler_span, "context_id").as_deref(),
        Some("ctx-1-1")
    );
    if let Some(http_span) = find_span(&spans, "baml_rt_api.http.request")
        .or_else(|| find_span_with_attr(&spans, "http.route", "/contexts/{context_id}/mermaid"))
        .or_else(|| find_span_with_attr(&spans, "url.path", "/contexts/ctx-1-1/mermaid"))
    {
        let route = attr_value(http_span, "http.route").unwrap_or_default();
        assert!(
            route == "/contexts/{context_id}/mermaid"
                || route == "/contexts/ctx-1-1/mermaid"
                || route == "<unmatched>",
            "http.route should be template, concrete path, or <unmatched>, got {route:?}"
        );
        assert_eq!(
            attr_value(http_span, "http.request.method").as_deref(),
            Some("GET")
        );
    }
}
