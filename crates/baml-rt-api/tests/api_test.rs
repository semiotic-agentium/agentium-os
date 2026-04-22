//! HTTP API tests: discovery, A2A forward, and error mapping.
//! Uses insta snapshots with selective redaction for variant parts (IDs, instance URLs, etc.).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baml_rt_a2a::AgentRegistry;
use baml_rt_api::{
    ClusterMode, ContextIndexError, ContextIndexRequest, ContextIndexService, ContextPickerPageDto,
    ConversationHistoryError, ConversationHistoryPageDto, ConversationHistoryProfile,
    ConversationHistoryRequest, ConversationHistoryService, MermaidError, MermaidService,
    ProvenanceOpsError, ProvenanceOpsService, api_router, api_router_with_services,
    api_router_with_services_and_deploy,
};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentInstanceId, AgentLister, AgentPackageName, AgentRouteKey,
    BamlRtError, BusStream, EventSchemaVersion, EventSourceKind, EventSubscription, Outcome,
    Result,
    event_subscription::{EventSourceKey, EventSourceKeyPrefix},
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, UuidId},
};
use baml_rt_provenance::{
    CallScope, GlobalEvent, LlmUsage, ProvEvent, ProvEventData, ProvenanceOpsQueryRequest,
    ProvenanceOpsQueryResponse, ProvenanceWriter, SurrealStoreBuilder, events::LlmDriftInfo,
};
use futures_util::stream;
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
            if looks_like_prov_activity_anchor(&s) {
                return V::String("[prov_activity_anchor]".to_string());
            }
            V::String(s)
        }
        V::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let redacted = match k.as_str() {
                    "instance" => V::String("[instance]".to_string()),
                    "type_url" => V::String("[type_url]".to_string()),
                    "event_id" | "a2a_activity_anchor" => {
                        V::String("[prov_activity_anchor]".to_string())
                    }
                    "timestamp_ms" => V::String("[timestamp_ms]".to_string()),
                    "timestampMs" => match &val {
                        V::Number(_) | V::String(_) => V::String("[timestamp_ms]".to_string()),
                        _ => redact_variant_parts(val),
                    },
                    "maxEventOrder" => V::String("[a2a_event_order]".to_string()),
                    "version" => match &val {
                        V::String(s) if s.starts_with("v1:") => {
                            V::String("[conversation_history_version]".to_string())
                        }
                        _ => redact_variant_parts(val),
                    },
                    // Monotonic / wall-clock ordering from store; varies every run.
                    "a2a_event_order" => V::String("[a2a_event_order]".to_string()),
                    // Wall-clock ms from Surreal / store; varies every run.
                    "prov_endTime" | "prov_startTime" => V::String("[timestamp_ms]".to_string()),
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

fn looks_like_prov_activity_anchor(s: &str) -> bool {
    // "prov-12345" (bare activity anchor)
    if s.strip_prefix("prov-")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
    {
        return true;
    }
    // "prov:v1:payload:payload:prov-<ns>:<kind>" compound payload ref
    s.starts_with("prov:v1:payload:")
}

async fn prov_test_router_with_history(
    registry: Arc<dyn AgentRegistry>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
    conversation_history: Option<Arc<dyn ConversationHistoryService>>,
    context_index: Option<Arc<dyn ContextIndexService>>,
) -> axum::Router {
    use baml_rt_config::SurrealConfigStore;
    use baml_rt_llm_config::EmptySecretResolver;
    use baml_rt_tools::InventoryCatalog;

    let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> = Arc::new(InventoryCatalog::new());
    let config_service: Arc<dyn baml_rt_config::ConfigService> = Arc::new(
        SurrealConfigStore::in_memory()
            .await
            .expect("in-memory config store for test"),
    );
    let secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver> =
        Arc::new(EmptySecretResolver);
    api_router_with_services(
        registry,
        None,
        None,
        provenance_ops,
        None, // planning
        None, // episode
        conversation_history,
        None, // conversation_history_events
        context_index,
        tool_catalog,
        config_service,
        secret_resolver,
        None,
        None,
    )
}

async fn prov_test_router(
    registry: Arc<dyn AgentRegistry>,
    provenance_ops: Option<Arc<dyn ProvenanceOpsService>>,
) -> axum::Router {
    prov_test_router_with_history(registry, provenance_ops, None, None).await
}

/// Snapshot-friendly representation of finished spans: name + attributes (variant values redacted).
const SPAN_REDACT_KEYS: &[&str] = &[
    "thread.id",
    "thread.name",
    "busy_ns",
    "idle_ns",
    // Surreal / registry paths and line numbers change with crate versions and machines.
    "code.filepath",
    "code.lineno",
    "code.namespace",
    // SurrealKV internal span attributes include instance-specific keys/vals per run.
    "key",
    "val",
    "rng",
    // In-memory config store uses UUID-scoped ns/db for test isolation; redact so
    // snapshots are stable across runs.
    "ns",
    "db",
];

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

/// Helper to find a span by name.
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
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

struct RealConversationHistory {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

struct MockContextIndex;

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

#[async_trait]
impl ConversationHistoryService for RealConversationHistory {
    async fn page(
        &self,
        request: &ConversationHistoryRequest,
    ) -> std::result::Result<ConversationHistoryPageDto, ConversationHistoryError> {
        use baml_rt_provenance::ProvenanceQueryApi;
        let rows = self
            .store
            .query_conversation_context(&request.context_id, None, request.task_id.as_ref())
            .await
            .map_err(|e| ConversationHistoryError::Other(Box::new(e)))?;
        let mut page = baml_rt_api::paginate_items(rows, request)?;
        if matches!(request.profile, ConversationHistoryProfile::Compact) {
            page.items = page
                .items
                .into_iter()
                .map(|item| baml_rt_api::profile_filter(item, request.profile))
                .collect();
        }
        Ok(page)
    }

    async fn delta_after_event_order(
        &self,
        request: &baml_rt_api::ConversationHistoryDeltaRequest,
    ) -> std::result::Result<ConversationHistoryPageDto, ConversationHistoryError> {
        use baml_rt_provenance::ProvenanceQueryApi;
        let rows = self
            .store
            .query_conversation_context_after(
                &request.context_id,
                request.after_event_order,
                Some(request.limit),
                request.task_id.as_ref(),
            )
            .await
            .map_err(|e| ConversationHistoryError::Other(Box::new(e)))?;
        let mut items = rows
            .into_iter()
            .map(baml_rt_api::ConversationHistoryItemDto::from)
            .collect::<Vec<_>>();
        if matches!(request.profile, ConversationHistoryProfile::Compact) {
            items = items
                .into_iter()
                .map(|item| baml_rt_api::profile_filter(item, request.profile))
                .collect();
        }
        let max_event_order = items.last().map(|item| item.timestamp_ms).unwrap_or(0);
        let version = baml_rt_api::page_version(&items);
        Ok(ConversationHistoryPageDto {
            context_id: request.context_id.as_str().to_string(),
            task_id: request.task_id.as_ref().map(|id| id.as_str().to_string()),
            version,
            max_event_order,
            items,
            next_cursor: None,
        })
    }
}

#[async_trait]
impl ContextIndexService for MockContextIndex {
    async fn page(
        &self,
        request: &ContextIndexRequest,
    ) -> std::result::Result<ContextPickerPageDto, ContextIndexError> {
        let all = [
            baml_rt_api::ContextPickerItemDto {
                context_id: "ctx-1".to_string(),
                latest_timestamp_ms: 20,
                preview: "first user message".to_string(),
            },
            baml_rt_api::ContextPickerItemDto {
                context_id: "ctx-2".to_string(),
                latest_timestamp_ms: 10,
                preview: "another thread".to_string(),
            },
        ];
        let end = request.offset.saturating_add(request.limit).min(all.len());
        let items = if request.offset < all.len() {
            all[request.offset..end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if end < all.len() {
            Some(
                baml_rt_api::ContextIndexCursorToken::encode_v1(
                    end,
                    request.agent_package.as_deref(),
                )
                .0,
            )
        } else {
            None
        };
        Ok(ContextPickerPageDto { items, next_cursor })
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

async fn seeded_provenance_store() -> Arc<baml_rt_provenance::SurrealProvenanceStore> {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
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
    store
        .add_event(ProvEvent::llm_call_completed_global(
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
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::llm_call_completed_global_with_drift(
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
            Some(Box::new(LlmDriftInfo {
                score: 0.618,
                severity: baml_rt_embedding::DriftSeverity::Warn,
                mode: baml_rt_embedding::DriftMode::Audit,
                warn_min_score: 0.5,
                block_min_score: 0.25,
                intent_text_preview: "Create a task titled Research".to_string(),
                response_text_preview: "Create task in list 901325431486".to_string(),
                step_text_preview: String::new(),
                plan_drift: None,
                citation_drift: None,
            })),
            vec![],
            vec![],
        ))
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
    let llm_fail_event_id = ActivityAnchorId::from_counter(500);
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: llm_fail_event_id.clone(),
            context_id: context.clone(),
            timestamp_ms: 3,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_b.clone(),
                },
                client: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                function_name: "DraftPrompt".to_string(),
                prompt: serde_json::json!({"input":"world"}),
                // Sparse metadata on purpose: linked PromptRejected should drive class.
                metadata: call_metadata(&agent_b, &msg_b, None),
                usage: LlmUsage::Known {
                    prompt_tokens: 20,
                    completion_tokens: 5,
                    total_tokens: 25,
                    cached_input_tokens: None,
                },
                duration_ms: 650,
                outcome: Outcome::Failure,
                drift: None,
                citations: vec![],
                resolved_citations: vec![],
            },
        }))
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
            Vec::new(),
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
            Vec::new(),
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
        content_hash: None,
        repository_version: None,
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        tools: vec![],
        baml_functions: vec![],
        description: None,
        capabilities: vec![],
        tags: vec![],
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
        content_hash: None,
        repository_version: None,
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        tools: vec![
            "system/internal_a2a".to_string(),
            "support/calculate".to_string(),
        ],
        baml_functions: vec![],
        description: description.map(str::to_string),
        capabilities: capabilities.into_iter().map(str::to_string).collect(),
        tags: vec![],
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
        content_hash: None,
        repository_version: None,
        agent_package: pkg.to_string(),
        agent_instance_id: inst.to_string(),
        tools: vec!["system/internal_a2a".to_string()],
        baml_functions: vec![],
        description: Some(format!("{name} subscribes to task-daemon events")),
        capabilities: vec!["a2a".to_string()],
        tags: vec![],
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
    let app = api_router(registry, None, None).await;

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
    let app = api_router(registry, None, None).await;

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
    let app = api_router(registry, None, None).await;

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
    let app = api_router(registry, None, None).await;

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
async fn get_agents_includes_provenance_fields_when_present() {
    let card = AgentCard {
        name: "ClickUp Agent".to_string(),
        version: "1.2.3".to_string(),
        content_hash: Some(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ),
        repository_version: Some(7),
        agent_package: "clickup-agent".to_string(),
        agent_instance_id: "default".to_string(),
        tools: vec!["support/clickup".to_string()],
        baml_functions: vec![],
        description: Some("ClickUp automation".to_string()),
        capabilities: vec!["a2a".to_string()],
        tags: vec!["clickup".to_string(), "tasks".to_string()],
        subscriptions: vec![],
    };
    let entries = vec![AgentDiscoveryEntry {
        agent_package: "clickup-agent".to_string(),
        agent_instance_id: "default".to_string(),
        name: "ClickUp Agent".to_string(),
        version: "1.2.3".to_string(),
        agent_card: card,
    }];
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(entries));
    let app = api_router(registry, None, None).await;

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
    assert_eq!(
        parsed
            .pointer("/0/agent_card/content_hash")
            .and_then(Value::as_str),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        parsed
            .pointer("/0/agent_card/repository_version")
            .and_then(Value::as_u64),
        Some(7)
    );
    assert_eq!(
        parsed
            .pointer("/0/agent_card/tags")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2)
    );
}

#[tokio::test]
async fn get_agents_omits_optional_provenance_fields_when_absent() {
    let entries = vec![discovery_entry("pkg-a", "default", "Agent A", "0.1.0")];
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(entries));
    let app = api_router(registry, None, None).await;

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
    assert!(
        parsed.pointer("/0/agent_card/content_hash").is_none(),
        "content_hash should be omitted when absent"
    );
    assert!(
        parsed.pointer("/0/agent_card/repository_version").is_none(),
        "repository_version should be omitted when absent"
    );
    assert!(
        parsed.pointer("/0/agent_card/tags").is_none(),
        "tags should be omitted when empty"
    );
}

#[tokio::test]
async fn get_openapi_json_returns_spec() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = api_router(registry, None, None).await;

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
async fn get_openapi_json_with_repository_returns_repo_paths() {
    let app = authed_test_router_with_repo(Some("secret"), ClusterMode::Cluster).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let spec: Value = serde_json::from_slice(&body).expect("valid openapi json");

    // Read routes are present
    assert!(
        spec.pointer("/paths/~1repository~1agents").is_some(),
        "/repository/agents should be in spec when repository is wired"
    );
    assert!(
        spec.pointer("/paths/~1repository~1entries").is_some(),
        "/repository/entries should be in spec when repository is wired"
    );

    // Mutation routes are present with RunnerToken security
    let publish_security = spec
        .pointer("/paths/~1repository~1publish/post/security")
        .expect("/repository/publish should have security");
    assert_eq!(
        publish_security,
        &serde_json::json!([{"RunnerToken": []}]),
        "publish must require RunnerToken"
    );

    let tags_security = spec
        .pointer("/paths/~1repository~1entries~1{hash}~1tags/post/security")
        .expect("/repository/entries/{hash}/tags POST should have security");
    assert_eq!(
        tags_security,
        &serde_json::json!([{"RunnerToken": []}]),
        "add_tag must require RunnerToken"
    );

    // Repository tag is present
    let tags = spec
        .get("tags")
        .and_then(Value::as_array)
        .expect("tags array");
    assert!(
        tags.iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some("repository")),
        "repository tag should be present when repository is wired"
    );
}

#[tokio::test]
async fn get_openapi_json_repo_less_omits_repository_paths() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let spec: Value = serde_json::from_slice(&body).expect("valid openapi json");

    // No /repository/* paths in the spec
    let paths = spec
        .get("paths")
        .and_then(Value::as_object)
        .expect("paths object");
    let repo_paths: Vec<&String> = paths
        .keys()
        .filter(|k| k.starts_with("/repository"))
        .collect();
    assert!(
        repo_paths.is_empty(),
        "repo-less router should not advertise /repository/* paths, found: {repo_paths:?}"
    );

    // No repository tag
    let tags = spec
        .get("tags")
        .and_then(Value::as_array)
        .expect("tags array");
    assert!(
        !tags
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some("repository")),
        "repository tag should not be present in repo-less router"
    );
}

#[tokio::test]
async fn post_a2a_returns_jsonrpc_array() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_handle_ok(vec![serde_json::json!({
                "jsonrpc": "2.0",
                "result": { "tasks": [], "totalSize": 0, "pageSize": 50 },
                "id": null
            })]),
    );
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}"#,
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
async fn post_dispatch_returns_buffered_ack() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_dispatch_ok(AgentDispatchAck {
                accepted: true,
                detail: Some("workflow intake accepted delivery".to_string()),
            }),
    );
    let app = api_router(registry, None, None).await;

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
async fn post_a2a_span_records_agent_identity_and_service_instance_fields() {
    let fixture = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_handle_ok(vec![serde_json::json!({
                "jsonrpc": "2.0",
                "result": { "tasks": [], "totalSize": 0, "pageSize": 50 },
                "id": null
            })]),
    );
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/default/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let spans = fixture.spans();
    let span = find_span(&spans, "baml_rt_api.post_a2a")
        .expect("baml_rt_api.post_a2a span emitted for POST /agents/.../a2a");
    assert_eq!(
        attr_value(span, "agent_package").as_deref(),
        Some("pkg"),
        "post_a2a span must carry agent_package attribute"
    );
    assert_eq!(
        attr_value(span, "agent_instance_id").as_deref(),
        Some("default"),
        "post_a2a span must carry agent_instance_id attribute"
    );
    assert_eq!(
        attr_value(span, "forwarded").as_deref(),
        Some("false"),
        "post_a2a span must mark forwarded=false in the single-runner path"
    );
    let ingress = attr_value(span, "ingress_service_instance_id")
        .expect("post_a2a span must carry ingress_service_instance_id");
    let serving = attr_value(span, "serving_service_instance_id")
        .expect("post_a2a span must carry serving_service_instance_id");
    assert!(!ingress.is_empty(), "ingress_service_instance_id is empty");
    assert_eq!(
        ingress, serving,
        "single-runner path must set ingress == serving service_instance_id"
    );
}

#[tokio::test]
async fn post_dispatch_span_records_agent_identity_and_service_instance_fields() {
    let fixture = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> = Arc::new(
        MockRegistry::with_entries(vec![discovery_entry("pkg", "default", "pkg", "1.0.0")])
            .with_dispatch_ok(AgentDispatchAck {
                accepted: true,
                detail: Some("workflow intake accepted delivery".to_string()),
            }),
    );
    let app = api_router(registry, None, None).await;

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
    assert_eq!(response.status(), StatusCode::OK);

    let spans = fixture.spans();
    let span = find_span(&spans, "baml_rt_api.post_dispatch")
        .expect("baml_rt_api.post_dispatch span emitted for POST /agents/.../dispatch");
    assert_eq!(attr_value(span, "agent_package").as_deref(), Some("pkg"));
    assert_eq!(
        attr_value(span, "agent_instance_id").as_deref(),
        Some("default")
    );
    assert_eq!(attr_value(span, "forwarded").as_deref(), Some("false"));
    let ingress = attr_value(span, "ingress_service_instance_id")
        .expect("post_dispatch span must carry ingress_service_instance_id");
    let serving = attr_value(span, "serving_service_instance_id")
        .expect("post_dispatch span must carry serving_service_instance_id");
    assert_eq!(ingress, serving);
}

#[tokio::test]
async fn post_a2a_bad_package_does_not_leak_raw_into_identity_span() {
    let fixture = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> =
        Arc::new(MockRegistry::with_entries(vec![discovery_entry(
            "pkg", "default", "pkg", "1.0.0",
        )]));
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/bad.pkg/default/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let spans = fixture.spans();
    for span in &spans {
        assert_ne!(
            span.name.as_ref(),
            "baml_rt_api.post_a2a",
            "post_a2a info span must not be emitted for bad identifiers — raw path would poison telemetry cardinality"
        );
        if let Some(package) = attr_value(span, "agent_package") {
            assert!(
                AgentPackageName::parse(&package).is_some(),
                "no span may carry raw public input as agent_package: got {package}"
            );
        }
    }
}

#[tokio::test]
async fn post_a2a_bad_instance_does_not_leak_raw_into_identity_span() {
    let fixture = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> =
        Arc::new(MockRegistry::with_entries(vec![discovery_entry(
            "pkg", "default", "pkg", "1.0.0",
        )]));
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/bad.instance/a2a")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","method":"tasks.list","params":{},"id":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let spans = fixture.spans();
    for span in &spans {
        assert_ne!(
            span.name.as_ref(),
            "baml_rt_api.post_a2a",
            "post_a2a info span must not be emitted for bad identifiers — raw path would poison telemetry cardinality"
        );
        if let Some(instance) = attr_value(span, "agent_instance_id") {
            assert!(
                AgentInstanceId::parse(&instance).is_some(),
                "no span may carry raw public input as agent_instance_id: got {instance}"
            );
        }
    }
}

#[tokio::test]
async fn post_dispatch_bad_package_does_not_leak_raw_into_identity_span() {
    let fixture = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> =
        Arc::new(MockRegistry::with_entries(vec![discovery_entry(
            "pkg", "default", "pkg", "1.0.0",
        )]));
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/bad.pkg/default/dispatch")
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
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let spans = fixture.spans();
    for span in &spans {
        assert_ne!(
            span.name.as_ref(),
            "baml_rt_api.post_dispatch",
            "post_dispatch info span must not be emitted for bad identifiers — raw path would poison telemetry cardinality"
        );
        if let Some(package) = attr_value(span, "agent_package") {
            assert!(
                AgentPackageName::parse(&package).is_some(),
                "no span may carry raw public input as agent_package: got {package}"
            );
        }
    }
}

#[tokio::test]
async fn post_dispatch_bad_instance_does_not_leak_raw_into_identity_span() {
    let fixture = OtelTestFixture::new();
    let registry: Arc<dyn AgentRegistry> =
        Arc::new(MockRegistry::with_entries(vec![discovery_entry(
            "pkg", "default", "pkg", "1.0.0",
        )]));
    let app = api_router(registry, None, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agents/pkg/bad.instance/dispatch")
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
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let spans = fixture.spans();
    for span in &spans {
        assert_ne!(
            span.name.as_ref(),
            "baml_rt_api.post_dispatch",
            "post_dispatch info span must not be emitted for bad identifiers — raw path would poison telemetry cardinality"
        );
        if let Some(instance) = attr_value(span, "agent_instance_id") {
            assert!(
                AgentInstanceId::parse(&instance).is_some(),
                "no span may carry raw public input as agent_instance_id: got {instance}"
            );
        }
    }
}

#[tokio::test]
async fn post_dispatch_rejects_empty_message_type() {
    let registry: Arc<dyn AgentRegistry> =
        Arc::new(MockRegistry::with_entries(vec![discovery_entry(
            "pkg", "default", "pkg", "1.0.0",
        )]));
    let app = api_router(registry, None, None).await;

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

#[tokio::test]
async fn get_mermaid_context_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let mermaid: Arc<dyn MermaidService> = Arc::new(MockMermaid::new(
        "sequenceDiagram\n    autonumber\n    actor User\n    participant agent\n    User->>agent: ping",
        "sequenceDiagram\n    autonumber\n    participant agent",
    ));
    let app = api_router(registry, Some(mermaid), None).await;

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
    let app = api_router(registry, Some(mermaid), None).await;

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
    )
    .await;
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
    )
    .await;
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
    let app = api_router(registry, None, None).await;
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
    )
    .await;
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
    )
    .await;
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
async fn get_conversation_history_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router_with_history(
        registry,
        None,
        Some(Arc::new(RealConversationHistory { store }) as Arc<dyn ConversationHistoryService>),
        None,
    )
    .await;
    let uri = format!("/contexts/{context_id}/conversation-history?limit=3&profile=full");

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
async fn get_conversation_history_pagination_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router_with_history(
        registry,
        None,
        Some(Arc::new(RealConversationHistory { store }) as Arc<dyn ConversationHistoryService>),
        None,
    )
    .await;

    let page_1_uri = format!("/contexts/{context_id}/conversation-history?limit=2");
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
    let body_1_json: Value = serde_json::from_slice(&body_1).unwrap();
    let cursor = body_1_json
        .get("nextCursor")
        .and_then(Value::as_str)
        .expect("nextCursor on first page")
        .to_string();
    let page_2_uri = format!("/contexts/{context_id}/conversation-history?limit=2&cursor={cursor}");

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
async fn get_conversation_history_invalid_cursor_returns_400_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router_with_history(
        registry,
        None,
        Some(Arc::new(RealConversationHistory { store }) as Arc<dyn ConversationHistoryService>),
        None,
    )
    .await;
    let uri = format!("/contexts/{context_id}/conversation-history?cursor=bad-token");

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
async fn get_context_index_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = prov_test_router_with_history(
        registry,
        None,
        None,
        Some(Arc::new(MockContextIndex) as Arc<dyn ContextIndexService>),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/contexts?limit=1&agentPackage=pkg")
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
async fn get_context_index_cursor_scope_mismatch_returns_400() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let app = prov_test_router_with_history(
        registry,
        None,
        None,
        Some(Arc::new(MockContextIndex) as Arc<dyn ContextIndexService>),
    )
    .await;

    let cursor = baml_rt_api::ContextIndexCursorToken::encode_v1(1, Some("pkg-a")).0;
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/contexts?agentPackage=pkg-b&cursor={cursor}"))
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
async fn get_provenance_llm_calls_pagination_returns_snapshot() {
    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let store = seeded_provenance_store().await;
    let context_id = ContextId::new(100, 1).to_string();
    let app = prov_test_router(
        registry,
        Some(Arc::new(RealProvenanceOps { store }) as Arc<dyn ProvenanceOpsService>),
    )
    .await;
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
    )
    .await;
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
    )
    .await;
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
    )
    .await;
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
    let app = api_router(registry, Some(mermaid), None).await;

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

// ── Auth boundary tests ──────────────────────────────────────────────

/// Build a router with explicit auth configuration for boundary tests.
async fn authed_test_router(token: Option<&str>, mode: ClusterMode) -> axum::Router {
    use std::sync::atomic::AtomicBool;

    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> =
        Arc::new(baml_rt_tools::InventoryCatalog::new());
    let config_service: Arc<dyn baml_rt_config::ConfigService> = Arc::new(
        baml_rt_config::SurrealConfigStore::in_memory()
            .await
            .expect("in-memory config store for auth test"),
    );
    let secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver> =
        Arc::new(baml_rt_llm_config::EmptySecretResolver);

    api_router_with_services_and_deploy(
        registry,
        None, // mermaid
        None, // context_metrics
        None, // provenance_ops
        None, // planning
        None, // episode
        None, // conversation_history
        None, // conversation_history_events
        None, // context_index
        None, // deployment_manager
        None, // repository_url
        None, // repository_service
        tool_catalog,
        config_service,
        secret_resolver,
        None, // runtime_secret_store
        Arc::new(AtomicBool::new(true)),
        token.map(String::from),
        mode,
        None, // web_dir
    )
}

#[tokio::test]
async fn config_put_secret_requires_auth_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/config/secrets/MY_KEY")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"link_from":"SOME_STORE_KEY"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_delete_secret_requires_auth_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/config/secrets/MY_KEY")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_put_bundle_requires_auth_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/config/llm")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_delete_bundle_requires_auth_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/config/llm")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn deploy_requires_auth_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/deploy")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"hash":"abc123"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn config_reads_require_auth_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;

    for path in [
        "/config",
        "/config/secrets-overview",
        "/config/secrets/store-keys",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "GET {path} should require auth in cluster mode"
        );
    }
}

#[tokio::test]
async fn config_reads_allowed_with_valid_token_in_cluster_mode() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;

    for path in [
        "/config",
        "/config/secrets-overview",
        "/config/secrets/store-keys",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("X-Runner-Token", "secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "GET {path} with valid token should pass auth"
        );
    }
}

#[tokio::test]
async fn config_reads_allowed_without_auth_in_standalone_mode() {
    let app = authed_test_router(None, ClusterMode::Standalone).await;

    for path in [
        "/config",
        "/config/secrets-overview",
        "/config/secrets/store-keys",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "GET {path} should not require auth in standalone mode"
        );
    }
}

#[tokio::test]
async fn public_routes_unaffected_by_auth() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;

    for path in ["/agents", "/healthz", "/readyz", "/openapi.json"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "GET {path} should return 200"
        );
    }
}

#[tokio::test]
async fn standalone_mode_allows_mutations_without_token() {
    let app = authed_test_router(None, ClusterMode::Standalone).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/config/llm")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "Standalone mode should not require auth"
    );
}

#[tokio::test]
async fn operator_routes_allow_with_valid_token() {
    let app = authed_test_router(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/config/llm")
                .header("content-type", "application/json")
                .header("X-Runner-Token", "secret")
                .body(Body::from(r#"{"model":"test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "Valid token should pass auth"
    );
}

/// Build a router with a real in-memory repository and auth configured.
async fn authed_test_router_with_repo(token: Option<&str>, mode: ClusterMode) -> axum::Router {
    use std::sync::atomic::AtomicBool;

    let registry: Arc<dyn AgentRegistry> = Arc::new(MockRegistry::with_entries(vec![]));
    let tool_catalog: Arc<dyn baml_rt_tools::ToolCatalog> =
        Arc::new(baml_rt_tools::InventoryCatalog::new());
    let config_service: Arc<dyn baml_rt_config::ConfigService> = Arc::new(
        baml_rt_config::SurrealConfigStore::in_memory()
            .await
            .expect("in-memory config store for auth test"),
    );
    let secret_resolver: Arc<dyn baml_rt_llm_config::SecretResolver> =
        Arc::new(baml_rt_llm_config::EmptySecretResolver);

    let store = Arc::new(
        baml_rt_repository::SurrealStore::open_in_memory()
            .await
            .expect("in-memory repository store"),
    );
    let repo_service = Arc::new(baml_rt_repository::RepositoryService::new(
        store.clone() as Arc<dyn baml_rt_repository::BlobStore>,
        store.clone() as Arc<dyn baml_rt_repository::MetadataStore>,
        store.clone() as Arc<dyn baml_rt_repository::LineageStore>,
        store as Arc<dyn baml_rt_repository::SearchStore>,
    ));

    api_router_with_services_and_deploy(
        registry,
        None, // mermaid
        None, // context_metrics
        None, // provenance_ops
        None, // planning
        None, // episode
        None, // conversation_history
        None, // conversation_history_events
        None, // context_index
        None, // deployment_manager
        None, // repository_url
        Some(repo_service),
        tool_catalog,
        config_service,
        secret_resolver,
        None, // runtime_secret_store
        Arc::new(AtomicBool::new(true)),
        token.map(String::from),
        mode,
        None, // web_dir
    )
}

#[tokio::test]
async fn repository_publish_requires_auth_in_cluster_mode() {
    let app = authed_test_router_with_repo(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/repository/publish")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repository_fork_requires_auth_in_cluster_mode() {
    let app = authed_test_router_with_repo(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/repository/fork")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repository_reads_allowed_without_auth_in_cluster_mode() {
    let app = authed_test_router_with_repo(Some("secret"), ClusterMode::Cluster).await;

    for path in ["/repository/agents", "/repository/entries"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "GET {path} should be public"
        );
    }
}

#[tokio::test]
async fn repository_add_tag_requires_auth_in_cluster_mode() {
    let app = authed_test_router_with_repo(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/repository/entries/abc123/tags")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tag":"stable"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repository_remove_tag_requires_auth_in_cluster_mode() {
    let app = authed_test_router_with_repo(Some("secret"), ClusterMode::Cluster).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/repository/entries/abc123/tags")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tag":"stable"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
