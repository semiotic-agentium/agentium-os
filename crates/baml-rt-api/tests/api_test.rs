//! HTTP API tests: discovery, A2A forward, and error mapping.
//! Uses insta snapshots with selective redaction for variant parts (IDs, instance URLs, etc.).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baml_rt_a2a::AgentRegistry;
use baml_rt_api::{
    MermaidError, MermaidService, ProvenanceOpsError, ProvenanceOpsService, api_router,
    api_router_with_services,
};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentLister, AgentRouteKey, BamlRtError, BusStream, EventSchemaVersion,
    EventSourceKey, EventSourceKeyPrefix, EventSourceKind, EventSubscription, Outcome, Result,
    ids::{AgentId, ContextId, EventId, ExternalId, MessageId, UuidId},
};
use baml_rt_provenance::{
    GraphqliteStoreBuilder, LlmUsage, ProvEvent, ProvenanceOpsQueryRequest,
    ProvenanceOpsQueryResponse, ProvenanceWriter, events::LlmDriftInfo,
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

fn schema_version(value: &str) -> EventSchemaVersion {
    EventSchemaVersion::parse(value).expect("valid schema version")
}

fn source_kind(value: &str) -> EventSourceKind {
    EventSourceKind::parse(value).expect("valid source kind")
}

fn source_key(value: &str) -> EventSourceKey {
    EventSourceKey::parse(value).expect("valid source key")
}

fn source_key_prefix(value: &str) -> EventSourceKeyPrefix {
    EventSourceKeyPrefix::parse(value).expect("valid source key prefix")
}

/// Redact variant parts of JSON (UUIDs, instance/type in problem bodies) for stable snapshots.
fn redact_variant_parts(v: Value) -> Value {
    use serde_json::Value as V;
    match v {
        V::String(s) => {
            if looks_like_uuid(&s) {
                return V::String("[uuid]".to_string());
            }
            if looks_like_prov_event_id(&s) {
                return V::String("[prov_event_id]".to_string());
            }
            V::String(s)
        }
        V::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let redacted = match k.as_str() {
                    "instance" => V::String("[instance]".to_string()),
                    "type_url" => V::String("[type_url]".to_string()),
                    "event_id" => V::String("[prov_event_id]".to_string()),
                    "timestamp_ms" => V::String("[timestamp_ms]".to_string()),
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

fn looks_like_prov_event_id(s: &str) -> bool {
    // "prov-12345" (bare event id)
    if s.strip_prefix("prov-")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
    {
        return true;
    }
    // "prov:v1:payload:payload:prov-<ns>:<kind>" compound payload ref
    s.starts_with("prov:v1:payload:")
}

fn prov_test_router(
    registry: Arc<dyn AgentRegistry>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
) -> axum::Router {
    use baml_rt_config::SqliteConfigStore;
    use baml_rt_llm_config::EmptySecretResolver;
    use baml_rt_tools::InventoryCatalog;

    let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> = Arc::new(InventoryCatalog::new());
    let config_service: Arc<dyn baml_rt_config::ConfigService> =
        Arc::new(SqliteConfigStore::in_memory().expect("in-memory config store for test"));
    let secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver> =
        Arc::new(EmptySecretResolver);
    api_router_with_services(
        registry,
        None,
        None,
        provenance_ops,
        None, // planning
        tool_catalog,
        config_service,
        secret_resolver,
        None,
        None,
    )
}

/// Snapshot-friendly representation of finished spans: name + attributes (variant values redacted).
const SPAN_REDACT_KEYS: &[&str] = &["thread.id", "thread.name", "busy_ns", "idle_ns"];

#[allow(dead_code)]
fn spans_snapshot(spans: &[opentelemetry_sdk::export::trace::SpanData]) -> Value {
    let refs: Vec<_> = spans.iter().collect();
    spans_snapshot_impl(&refs, SPAN_REDACT_KEYS)
}

fn spans_snapshot_for_test(
    spans: &[opentelemetry_sdk::export::trace::SpanData],
    thread_name: &str,
) -> Value {
    let filtered: Vec<_> = spans
        .iter()
        .filter(|s| attr_value(s, "thread.name").as_deref() == Some(thread_name))
        .collect();
    spans_snapshot_impl(&filtered, SPAN_REDACT_KEYS)
}

fn spans_snapshot_impl(
    spans: &[&opentelemetry_sdk::export::trace::SpanData],
    redact_keys: &[&str],
) -> Value {
    use opentelemetry::Value as OtelValue;
    let redact: std::collections::HashSet<&str> = redact_keys.iter().copied().collect();
    let arr: Vec<Value> = spans
        .iter()
        .map(|s| {
            let mut attrs = serde_json::Map::new();
            for kv in &s.attributes {
                let key = kv.key.as_str().to_string();
                let val = if redact.contains(kv.key.as_str()) {
                    Value::String("[redacted]".to_string())
                } else {
                    match &kv.value {
                        OtelValue::String(v) => {
                            let v = v.to_string();
                            Value::String(if looks_like_uuid(&v) {
                                "[uuid]".to_string()
                            } else {
                                v
                            })
                        }
                        OtelValue::Bool(b) => Value::Bool(*b),
                        OtelValue::I64(i) => Value::Number(serde_json::Number::from(*i)),
                        OtelValue::F64(f) => serde_json::Number::from_f64(*f)
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                        _ => Value::String(kv.value.to_string()),
                    }
                };
                attrs.insert(key, val);
            }
            serde_json::json!({ "name": s.name.as_ref(), "attributes": attrs })
        })
        .collect();
    Value::Array(arr)
}

/// Mock registry for testing: fixed list and configurable A2A response.
struct MockRegistry {
    entries: Vec<AgentDiscoveryEntry>,
    handle_ok: Option<Vec<A2aStreamChunk>>,
    /// When set, yield each value with a delay between yields (for no-buffering tests).
    handle_delayed: Option<Vec<A2aStreamChunk>>,
    handle_err_message: Option<String>,
    dispatch_ok: Option<AgentDispatchAck>,
    dispatch_err_message: Option<String>,
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

/// Helper to find a span by name; reserved for future span assertions.
#[allow(dead_code)]
fn find_span<'a>(
    spans: &'a [opentelemetry_sdk::export::trace::SpanData],
    name: &str,
) -> Option<&'a opentelemetry_sdk::export::trace::SpanData> {
    spans.iter().find(|span| span.name.as_ref() == name)
}

/// Helper to find a span by attribute key/value; reserved for future span assertions.
#[allow(dead_code)]
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

struct RealProvenanceOps {
    store: Arc<baml_rt_provenance::GraphqliteProvenanceStore>,
}

#[async_trait]
impl ProvenanceOpsService for RealProvenanceOps {
    async fn query(
        &self,
        request: ProvenanceOpsQueryRequest,
    ) -> std::result::Result<ProvenanceOpsQueryResponse, ProvenanceOpsError> {
        use baml_rt_provenance::ProvenanceOpsQuery;
        self.store
            .query_ops(request)
            .await
            .map_err(|e| ProvenanceOpsError::Other(Box::new(e)))
    }
}

fn call_metadata(agent_id: &AgentId, message_id: &MessageId, error: Option<&str>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "agent_id".to_string(),
        Value::String(agent_id.as_str().to_string()),
    );
    map.insert(
        "message_id".to_string(),
        Value::String(message_id.as_str().to_string()),
    );
    if let Some(error) = error {
        map.insert("error".to_string(), Value::String(error.to_string()));
    }
    Value::Object(map)
}

async fn seeded_provenance_store() -> Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path: PathBuf = std::env::temp_dir().join(format!(
        "baml-rt-api-provenance-{}-{unique}.db",
        std::process::id()
    ));
    let store = GraphqliteStoreBuilder::file(&path)
        .build()
        .expect("build store");
    let context = ContextId::new(100, 1);
    let agent_a =
        AgentId::from_uuid(UuidId::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
    let agent_b =
        AgentId::from_uuid(UuidId::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
    let msg_a = MessageId::from_external(ExternalId::new("msg-a".to_string()));
    let msg_b = MessageId::from_external(ExternalId::new("msg-b".to_string()));
    let msg_c = MessageId::from_external(ExternalId::new("msg-c".to_string()));
    let mut msg_meta = HashMap::new();
    msg_meta.insert("channel".to_string(), "api-test".to_string());

    store
        .add_event(ProvEvent::message_received_global(
            context.clone(),
            msg_a.clone(),
            "ROLE_USER".to_string(),
            vec!["run analysis".to_string()],
            Some(msg_meta.clone()),
            agent_a.clone(),
            1,
        ))
        .await
        .unwrap();
    // Explicit timestamps (1, 2) so pagination sorted by timestamp_ms is deterministic.
    store
        .add_event(
            ProvEvent::llm_call_completed_global(
                context.clone(),
                msg_a.clone(),
                "openai".to_string(),
                "gpt-4o-mini".to_string(),
                "SummarizePrompt".to_string(),
                serde_json::json!({"input":"hello"}),
                call_metadata(&agent_a, &msg_a, None),
                LlmUsage::Known {
                    prompt_tokens: 12,
                    completion_tokens: 8,
                    total_tokens: 20,
                    cached_input_tokens: None,
                },
                180,
                Outcome::Success,
            )
            .with_event_id(EventId::from_counter(100))
            .with_timestamp_ms(1),
        )
        .await
        .unwrap();
    store
        .add_event(
            ProvEvent::llm_call_completed_global_with_drift(
                context.clone(),
                msg_a.clone(),
                "openai".to_string(),
                "gpt-4o-mini".to_string(),
                "SummarizePrompt".to_string(),
                serde_json::json!({"input":"hello with drift"}),
                call_metadata(&agent_a, &msg_a, None),
                LlmUsage::Known {
                    prompt_tokens: 10,
                    completion_tokens: 7,
                    total_tokens: 17,
                    cached_input_tokens: None,
                },
                181,
                Outcome::Success,
                Some(LlmDriftInfo {
                    score: 0.618,
                    severity: "warn".to_string(),
                    mode: "audit".to_string(),
                    warn_min_score: 0.5,
                    block_min_score: 0.25,
                    intent_text_preview: "Create a task titled Research".to_string(),
                    response_text_preview: "Create task in list 901325431486".to_string(),
                }),
            )
            .with_event_id(EventId::from_counter(200))
            .with_timestamp_ms(2),
        )
        .await
        .unwrap();
    store
        .add_event(ProvEvent::tool_call_completed_global(
            context.clone(),
            msg_a.clone(),
            "support/calculate".to_string(),
            Some("CalcPrompt".to_string()),
            serde_json::json!({"expression":"2+2"}),
            call_metadata(&agent_a, &msg_a, Some("timeout while calling tool")),
            420,
            Outcome::Failure,
            None,
        ))
        .await
        .unwrap();

    store
        .add_event(ProvEvent::message_received_global(
            context.clone(),
            msg_b.clone(),
            "ROLE_USER".to_string(),
            vec!["secondary flow".to_string()],
            Some(msg_meta),
            agent_b.clone(),
            2,
        ))
        .await
        .unwrap();
    let llm_fail_event_id = EventId::from_counter(500);
    store
        .add_event(
            ProvEvent::llm_call_completed_global(
                context.clone(),
                msg_b.clone(),
                "anthropic".to_string(),
                "claude-3-7-sonnet".to_string(),
                "DraftPrompt".to_string(),
                serde_json::json!({"input":"world"}),
                // Sparse metadata on purpose: linked PromptRejected should drive class.
                call_metadata(&agent_b, &msg_b, None),
                LlmUsage::Known {
                    prompt_tokens: 20,
                    completion_tokens: 5,
                    total_tokens: 25,
                    cached_input_tokens: None,
                },
                650,
                Outcome::Failure,
            )
            .with_event_id(llm_fail_event_id.clone())
            .with_timestamp_ms(3),
        )
        .await
        .unwrap();
    store
        .add_event(ProvEvent::prompt_rejected_global(
            context.clone(),
            msg_b.clone(),
            llm_fail_event_id,
            "BAML validation failed: invalid response schema".to_string(),
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::message_sent_global(
            context.clone(),
            msg_b.clone(),
            "ROLE_AGENT".to_string(),
            vec!["BAML validation failed: invalid response schema".to_string()],
            None,
            agent_b.clone(),
            4,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::message_received_global(
            context.clone(),
            msg_c.clone(),
            "ROLE_USER".to_string(),
            vec!["third flow".to_string()],
            None,
            agent_a.clone(),
            5,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::tool_call_completed_global(
            context.clone(),
            msg_c.clone(),
            "support/delegate".to_string(),
            Some("DelegatePrompt".to_string()),
            serde_json::json!({"objective":"narrow evidence fixture"}),
            call_metadata(&agent_a, &msg_c, None),
            240,
            Outcome::Failure,
            None,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::message_sent_global(
            context.clone(),
            msg_c,
            "ROLE_AGENT".to_string(),
            vec!["authentication failed: 401 unauthorized invalid api key".to_string()],
            None,
            agent_a,
            6,
        ))
        .await
        .unwrap();

    store
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
            dispatch_ok: None,
            dispatch_err_message: None,
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

    fn with_dispatch_ok(mut self, ack: AgentDispatchAck) -> Self {
        self.dispatch_ok = Some(ack);
        self
    }

    /// Builder helper for tests that assert 404/not-found; reserved for alternative test paths.
    #[allow(dead_code)] // test-only builder path
    fn with_handle_err_not_found(mut self, message: String) -> Self {
        self.handle_err_message = Some(message);
        self
    }

    #[allow(dead_code)] // test-only builder path
    fn with_dispatch_err_not_found(mut self, message: String) -> Self {
        self.dispatch_err_message = Some(message);
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

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        _request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck> {
        if let Some(ref cell) = self.key_captured {
            *cell.lock().unwrap() = Some(key.clone());
        }
        if let Some(ref ack) = self.dispatch_ok {
            return Ok(ack.clone());
        }
        if let Some(ref msg) = self.dispatch_err_message {
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
        baml_functions: vec![],
        description: None,
        capabilities: vec![],
        subscriptions: vec![],
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
        baml_functions: vec![],
        description: description.map(str::to_string),
        capabilities: capabilities.into_iter().map(str::to_string).collect(),
        subscriptions: vec![],
    };
    AgentDiscoveryEntry {
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        agent_card: card,
    }
}

fn discovery_entry_with_subscriptions(
    pkg: &str,
    inst: &str,
    name: &str,
    version: &str,
    subscriptions: Vec<EventSubscription>,
) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        tools: vec!["system/internal_a2a".to_string()],
        baml_functions: vec![],
        description: Some(format!("{name} subscribes to task-daemon events")),
        capabilities: vec!["a2a".to_string()],
        subscriptions,
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
async fn get_agents_returns_declared_subscriptions_when_present() {
    let entries = vec![discovery_entry_with_subscriptions(
        "workflow-subscriber",
        "default",
        "Workflow Subscriber",
        "0.1.0",
        vec![EventSubscription {
            schema_versions: vec![schema_version("task-daemon.interpretation.v1")],
            source_kinds: vec![source_kind("slack"), source_kind("clickup")],
            source_keys: vec![source_key("slack:C123")],
            source_key_prefixes: vec![source_key_prefix("clickup:list:")],
        }],
    )];
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

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("valid json response");
    let subscriptions = parsed
        .pointer("/0/agent_card/subscriptions")
        .and_then(|value| value.as_array())
        .expect("subscriptions array should be present");
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(
        subscriptions[0]
            .get("schema_versions")
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        subscriptions[0]
            .get("source_kinds")
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        subscriptions[0]
            .get("source_keys")
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        subscriptions[0]
            .get("source_key_prefixes")
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );
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

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body).into_owned();

    let snapshot = serde_json::json!({
        "status": status.as_u16(),
        "content_type": content_type,
        "body": body_str,
    });
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn post_dispatch_returns_buffered_ack() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_dispatch_ok(AgentDispatchAck {
                accepted: true,
                detail: Some("workflow intake accepted delivery".to_string()),
            }),
    );
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/dispatch")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{
                        "routing_key":"slack:intake",
                        "message_type":"task-daemon.interpretation.v1",
                        "messages":[{"schema_version":"task-daemon.interpretation.v1"}]
                    }"#,
                ))
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
async fn post_dispatch_rejects_empty_message_type() {
    let registry: Arc<dyn AgentRegistry> =
        Arc::new(MockRegistry::with_entries(vec![discovery_entry(
            "pkg", "default", "pkg", "1.0.0",
        )]));
    let app = api_router(registry, None, None);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/dispatch")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{
                        "routing_key":"slack:intake",
                        "message_type":"   ",
                        "messages":[{"schema_version":"task-daemon.interpretation.v1"}]
                    }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body_text.contains("message_type must be non-empty"),
        "unexpected response body: {body_text}"
    );
}

/// Server must not buffer the A2A stream: events arrive incrementally.
/// Mock yields three items with 80ms delay between each; first SSE event must arrive within 200ms.
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

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body = response.into_body();
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    let mut events_received = 0u32;
    let mut first_event_elapsed_ms: Option<u128> = None;
    let start = std::time::Instant::now();
    const FIRST_EVENT_MAX_MS: u128 = 200;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("body chunk");
        buf.extend_from_slice(&chunk);
        let mut line_start = 0;
        while let Some(offset) = buf[line_start..].iter().position(|&b| b == b'\n') {
            let line_end = line_start + offset + 1;
            let line = &buf[line_start..line_end];
            if line.starts_with(b"data:") && line.len() > 5 {
                events_received += 1;
                if first_event_elapsed_ms.is_none() {
                    first_event_elapsed_ms = Some(start.elapsed().as_millis());
                }
            }
            line_start = line_end;
        }
        buf.drain(..line_start);
    }

    let first_within_limit = first_event_elapsed_ms.map(|ms| ms < FIRST_EVENT_MAX_MS);
    assert!(
        first_within_limit == Some(true),
        "first SSE event must arrive within {FIRST_EVENT_MAX_MS}ms (no server buffering); got {:?}",
        first_event_elapsed_ms
    );
    let snapshot = serde_json::json!({
        "status": status.as_u16(),
        "content_type": content_type,
        "events_received": events_received,
        "first_event_within_limit": true,
    });
    insta::assert_json_snapshot!(snapshot);
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
async fn get_provenance_llm_calls_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let uri = format!(
        "/provenance/llm-calls?contextId={}&agentId={}&groupBy=agent_id,model",
        context_id,
        agent_id.as_str()
    );

    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
async fn get_provenance_llm_calls_nests_drift_fields() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let uri = format!(
        "/provenance/llm-calls?contextId={}&agentId={}&sortBy=duration_ms&sortDir=desc&pageSize=10",
        context_id,
        agent_id.as_str()
    );

    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).expect("json body");
    let rows = json
        .get("rows")
        .and_then(Value::as_array)
        .expect("rows array");
    let drift_row = rows
        .iter()
        .find(|row| row.get("drift").is_some())
        .expect("row with drift");

    let drift = drift_row
        .get("drift")
        .and_then(Value::as_object)
        .expect("drift object");
    let score = drift
        .get("score")
        .and_then(Value::as_f64)
        .expect("drift score");
    assert!((score - 0.618).abs() < 0.001, "unexpected score: {score}");
    assert_eq!(drift.get("severity"), Some(&serde_json::json!("warn")));
    assert_eq!(drift.get("mode"), Some(&serde_json::json!("audit")));
    assert_eq!(drift.get("warnMinScore"), Some(&serde_json::json!(0.5)));
    assert_eq!(drift.get("blockMinScore"), Some(&serde_json::json!(0.25)));
    assert!(drift.get("intentTextPreview").is_some());
    assert!(drift.get("responseTextPreview").is_some());
    assert!(drift_row.get("drift_score").is_none());
    assert!(drift_row.get("drift_severity").is_none());
    assert!(drift_row.get("intent_text_preview").is_none());
}

#[tokio::test]
async fn get_provenance_aggregates_unavailable_returns_501_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = api_router(registry, None, None);
    let context_id = ContextId::new(1, 1).to_string();
    let uri = format!("/provenance/aggregates?contextId={context_id}");

    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
async fn get_provenance_tool_calls_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let uri = format!(
        "/provenance/tool-calls?contextId={}&agentId={}&groupBy=agent_id,tool_name&outcome=failed_only",
        context_id,
        agent_id.as_str()
    );

    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
async fn get_provenance_messages_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let uri = format!(
        "/provenance/messages?contextId={context_id}&groupBy=agent_id,baml_prompt&sortBy=total_processing_ms&sortDir=desc"
    );

    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
async fn get_provenance_llm_calls_pagination_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let page_1_uri = format!(
        "/provenance/llm-calls?contextId={context_id}&sortBy=timestamp_ms&sortDir=asc&pageSize=1"
    );
    let page_2_uri = format!(
        "/provenance/llm-calls?contextId={context_id}&sortBy=timestamp_ms&sortDir=asc&pageSize=1&cursor=1"
    );

    let response_page_1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(page_1_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status_1 = response_page_1.status();
    let body_1 = axum::body::to_bytes(response_page_1.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_page_2 = app
        .oneshot(
            Request::builder()
                .uri(page_2_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status_2 = response_page_2.status();
    let body_2 = axum::body::to_bytes(response_page_2.into_body(), usize::MAX)
        .await
        .unwrap();

    let snapshot = serde_json::json!({
        "page1": response_snapshot(status_1, &body_1),
        "page2": response_snapshot(status_2, &body_2),
    });
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn get_provenance_llm_calls_filter_sort_and_drilldown_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let drilldown_agent =
        AgentId::from_uuid(UuidId::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap());
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let filtered_uri = format!(
        "/provenance/llm-calls?contextId={context_id}&provider=anthropic&outcome=failed_only&sortBy=duration_ms&sortDir=desc&groupBy=agent_id,provider,model,baml_prompt"
    );
    let drilldown_uri = format!(
        "/provenance/llm-calls?contextId={}&agentId={}&provider=anthropic&model=claude-3-7-sonnet&bamlPrompt=DraftPrompt&sortBy=timestamp_ms&sortDir=asc&pageSize=5",
        context_id,
        drilldown_agent.as_str()
    );

    let filtered = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(filtered_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let filtered_status = filtered.status();
    let filtered_body = axum::body::to_bytes(filtered.into_body(), usize::MAX)
        .await
        .unwrap();

    let drilled = app
        .oneshot(
            Request::builder()
                .uri(drilldown_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let drilled_status = drilled.status();
    let drilled_body = axum::body::to_bytes(drilled.into_body(), usize::MAX)
        .await
        .unwrap();

    let snapshot = serde_json::json!({
        "filtered": response_snapshot(filtered_status, &filtered_body),
        "drilldown": response_snapshot(drilled_status, &drilled_body),
    });
    insta::assert_json_snapshot!(snapshot);
}

#[tokio::test]
async fn get_provenance_tool_calls_filter_sort_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap());
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let uri = format!(
        "/provenance/tool-calls?contextId={}&agentId={}&toolName=support/calculate&sortBy=duration_ms&sortDir=desc&groupBy=agent_id,tool_name",
        context_id,
        agent_id.as_str()
    );

    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
async fn get_provenance_failure_evidence_linked_modes_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    );
    let llm_uri = format!(
        "/provenance/llm-calls?contextId={context_id}&provider=anthropic&outcome=failed_only&sortBy=timestamp_ms&sortDir=asc&pageSize=5"
    );
    let tool_uri = format!(
        "/provenance/tool-calls?contextId={context_id}&toolName=support/delegate&outcome=failed_only&sortBy=timestamp_ms&sortDir=asc&pageSize=5"
    );

    let llm_response = app
        .clone()
        .oneshot(Request::builder().uri(llm_uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let llm_status = llm_response.status();
    let llm_body = axum::body::to_bytes(llm_response.into_body(), usize::MAX)
        .await
        .unwrap();

    let tool_response = app
        .oneshot(
            Request::builder()
                .uri(tool_uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let tool_status = tool_response.status();
    let tool_body = axum::body::to_bytes(tool_response.into_body(), usize::MAX)
        .await
        .unwrap();

    let snapshot = serde_json::json!({
        "llm_linked_prompt_rejected": response_snapshot(llm_status, &llm_body),
        "tool_linked_emitted_message": response_snapshot(tool_status, &tool_body),
    });
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

    let spans = otel.spans();
    let snapshot = serde_json::json!({
        "status": response.status().as_u16(),
        "spans": spans_snapshot_for_test(&spans, "get_mermaid_context_emits_http_and_handler_spans"),
    });
    insta::assert_json_snapshot!(snapshot);
}
