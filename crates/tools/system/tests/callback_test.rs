// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use baml_derive_core::JsonSchemaType;
use baml_rt::{
    baml::BamlRuntimeManager,
    interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor},
};
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentLister,
    BamlRtError, BusStream, ContextId, DISPATCH_METADATA_SCHEDULING_CONTEXT_ID,
    DISPATCH_METADATA_SCHEDULING_TASK_ID, Result,
    context::{self, InvocationScope},
    ids::{AgentId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_tools::{
    EventProducerBuildContext, ProducerCheckpoint, SessionPlanTypeName, ToolRegistry, ToolStep,
};
use baml_tools_system::{
    CallbackToolInput, SystemBundle,
    callback_delivery_gate::{
        CallbackDeliveryGate, clear_callback_delivery_gate, install_callback_delivery_gate,
    },
    callback_producer::{
        CALLBACK_EVENT_ROUTING_KEY, CALLBACK_EVENT_SCHEMA_VERSION, CALLBACK_SOURCE_KIND,
        build_callback_event_producers,
    },
    callback_store::{
        CallbackStore, CancelCallbackSelector, ScheduleCallbackRequest, ScheduleCallbackResult,
        StoredCallback, clear_callback_store, install_callback_store,
    },
    metadata::system_callback_metadata,
};
use futures_util::stream;
use serde_json::{Value, json};

struct MockA2aHandler;

#[async_trait]
impl A2aRequestHandler for MockA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        Ok(Box::pin(stream::empty::<A2aStreamChunk>()))
    }
}

#[derive(Default)]
struct EmptyAgentList;

impl AgentLister for EmptyAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        Vec::new()
    }
}

fn suite_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn test_context_id() -> ContextId {
    ContextId::new(100, 7)
}

fn test_task_id() -> TaskId {
    TaskId::from_external(ExternalId::new("task-callback-test"))
}

fn test_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap())
}

fn test_registry() -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_bundle(SystemBundle::new(
            Arc::new(EmptyAgentList),
            registry.clone(),
            Arc::new(MockA2aHandler),
        ))
        .unwrap();
    registry
}

async fn invoke_callback_tool(
    registry: &ToolRegistry,
    context_id: &ContextId,
    agent_id: &AgentId,
    task_id: Option<&TaskId>,
    input: Value,
) -> ToolStep {
    let session_id = registry
        .open_session_scoped("system/callback", json!({}), context_id, agent_id, task_id)
        .await
        .unwrap();
    registry.session_send(&session_id, input).await.unwrap();
    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Error { error } => registry
            .session_abort(&session_id, Some(error.message.clone()))
            .await
            .unwrap(),
        _ => registry.session_finish(&session_id).await.unwrap(),
    }
    step
}

fn expect_done_output(step: ToolStep) -> Value {
    match step {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected Done(Some(output)), got {other:?}"),
    }
}

fn expect_error_message(step: ToolStep) -> String {
    match step {
        ToolStep::Error { error } => error.message,
        other => panic!("expected Error, got {other:?}"),
    }
}

struct CallbackGlobalsCleanup;

impl Drop for CallbackGlobalsCleanup {
    fn drop(&mut self) {
        clear_callback_store();
        clear_callback_delivery_gate();
    }
}

struct TempSchemaDir {
    path: PathBuf,
}

impl TempSchemaDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "callback-baml-schema-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).expect("create temporary callback schema dir");
        Self { path }
    }
}

impl Drop for TempSchemaDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const CALLBACK_RUNTIME_BAML: &str =
    include_str!("../../../../tests/fixtures/agents/dispatch-echo/baml_src/_baml_runtime.baml");

const CALLBACK_PLAN_PROMPT_BAML: &str = r##"
function PlanCallbackSchedule(token: string) -> SystemCallbackSessionPlan {
  client DefaultClient
  prompt #"
    Schedule a callback for {{ token }}.
    {{ ctx.output_format }}
  "#
}

retry_policy ParseRetry {
  max_retries 1
  strategy {
    type constant_delay
    delay_ms 1
  }
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
"##;

fn write_callback_plan_schema_fixture() -> TempSchemaDir {
    let temp_dir = TempSchemaDir::new("plan");
    let baml_src_dir = temp_dir.path.join("baml_src");
    fs::create_dir_all(&baml_src_dir).expect("create temp baml_src dir");
    fs::write(
        baml_src_dir.join("_baml_runtime.baml"),
        CALLBACK_RUNTIME_BAML,
    )
    .expect("write generated runtime BAML");
    fs::write(
        baml_src_dir.join("callback_plan_prompt.baml"),
        CALLBACK_PLAN_PROMPT_BAML,
    )
    .expect("write callback plan prompt BAML");
    temp_dir
}

struct StubCallbackPlanInterceptor;

#[async_trait]
impl LLMInterceptor for StubCallbackPlanInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        if context.function_id.prompt_name().as_str() == "PlanCallbackSchedule" {
            return Ok(InterceptorDecision::Substitute(json!({
                "step": {
                    "op": "Send",
                    "input": {
                        "op": "schedule",
                        "after_ms": 0,
                        "source_key": "coordinator-agent:follow-up",
                        "payload": {
                            "__baml_opaque_json": "{\"kind\":\"wrapped\",\"count\":2}"
                        }
                    },
                    "citations": []
                },
                "citations": []
            })));
        }
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &Result<serde_json::Value>,
        _duration_ms: u64,
    ) {
    }
}

#[derive(Clone)]
struct MemoryCallbackStore {
    next_id: Arc<AtomicU64>,
    rows: Arc<tokio::sync::Mutex<Vec<MemoryCallbackRow>>>,
}

#[derive(Clone)]
struct MemoryCallbackRow {
    callback: StoredCallback,
    status: &'static str,
    emitted_at_unix_ms: Option<u64>,
}

impl Default for MemoryCallbackStore {
    fn default() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            rows: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }
}

impl MemoryCallbackStore {
    async fn snapshot(&self) -> Vec<MemoryCallbackRow> {
        self.rows.lock().await.clone()
    }
}

struct ToggleDeliveryGate {
    allow: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl CallbackDeliveryGate for ToggleDeliveryGate {
    async fn can_emit_callback(&self, _callback: &StoredCallback) -> Result<bool> {
        Ok(self.allow.load(Ordering::Relaxed))
    }
}

#[async_trait]
impl CallbackStore for MemoryCallbackStore {
    async fn schedule_callback(
        &self,
        request: ScheduleCallbackRequest,
    ) -> Result<ScheduleCallbackResult> {
        let mut rows = self.rows.lock().await;
        if let Some(dedupe_key) = &request.dedupe_key
            && let Some(existing) = rows.iter().find(|row| {
                row.status == "pending"
                    && row.emitted_at_unix_ms.is_none()
                    && row.callback.source_key == request.source_key
                    && row.callback.dedupe_key.as_deref() == Some(dedupe_key.as_str())
            })
        {
            return Ok(ScheduleCallbackResult {
                callback: existing.callback.clone(),
                created: false,
            });
        }

        let callback = StoredCallback {
            callback_id: format!("cb-{}", self.next_id.fetch_add(1, Ordering::Relaxed)),
            source_key: request.source_key,
            dedupe_key: request.dedupe_key,
            payload: request.payload,
            scheduled_for_unix_ms: request.scheduled_for_unix_ms,
            requested_at_unix_ms: request.requested_at_unix_ms,
            context_id: request.context_id,
            task_id: request.task_id,
            scheduling_context_id: request.scheduling_context_id,
            scheduling_task_id: request.scheduling_task_id,
            requesting_agent_id: request.requesting_agent_id,
            requesting_message_id: request.requesting_message_id,
        };
        rows.push(MemoryCallbackRow {
            callback: callback.clone(),
            status: "pending",
            emitted_at_unix_ms: None,
        });
        Ok(ScheduleCallbackResult {
            callback,
            created: true,
        })
    }

    async fn cancel_callback(
        &self,
        selector: CancelCallbackSelector,
    ) -> Result<Option<StoredCallback>> {
        let mut rows = self.rows.lock().await;
        let found = rows.iter_mut().find(|row| {
            if row.status != "pending" {
                return false;
            }
            if row.emitted_at_unix_ms.is_some() {
                return false;
            }
            match &selector {
                CancelCallbackSelector::CallbackId(callback_id) => {
                    row.callback.callback_id == *callback_id
                }
                CancelCallbackSelector::DedupeKey {
                    source_key,
                    dedupe_key,
                } => {
                    row.callback.source_key == *source_key
                        && row.callback.dedupe_key.as_deref() == Some(dedupe_key.as_str())
                }
            }
        });
        match found {
            Some(row) => {
                row.status = "cancelled";
                Ok(Some(row.callback.clone()))
            }
            None => Ok(None),
        }
    }

    async fn list_due_callbacks(
        &self,
        now_unix_ms: u64,
        limit: usize,
    ) -> Result<Vec<StoredCallback>> {
        let mut callbacks = self
            .rows
            .lock()
            .await
            .iter()
            .filter(|row| {
                row.status == "pending" && row.callback.scheduled_for_unix_ms <= now_unix_ms
            })
            .map(|row| row.callback.clone())
            .collect::<Vec<_>>();
        callbacks.sort_by(|left, right| {
            left.scheduled_for_unix_ms
                .cmp(&right.scheduled_for_unix_ms)
                .then_with(|| left.callback_id.cmp(&right.callback_id))
        });
        callbacks.truncate(limit);
        Ok(callbacks)
    }

    async fn mark_callbacks_emitted(
        &self,
        callback_ids: &[String],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<String>> {
        let mut rows = self.rows.lock().await;
        let mut emitted_ids = Vec::new();
        for row in rows.iter_mut() {
            if row.status == "pending" && callback_ids.contains(&row.callback.callback_id) {
                if row.emitted_at_unix_ms.is_none() {
                    row.emitted_at_unix_ms = Some(emitted_at_unix_ms);
                }
                emitted_ids.push(row.callback.callback_id.clone());
            }
        }
        Ok(emitted_ids)
    }

    async fn mark_callbacks_delivered(
        &self,
        callback_ids: &[String],
        _delivered_at_unix_ms: u64,
    ) -> Result<()> {
        let mut rows = self.rows.lock().await;
        for row in rows.iter_mut() {
            if row.status == "pending" && callback_ids.contains(&row.callback.callback_id) {
                row.status = "delivered";
            }
        }
        Ok(())
    }
}

#[test]
fn callback_tool_input_schema_requires_op_discriminator() {
    let schema = CallbackToolInput::json_schema_inline();
    let variants = schema["oneOf"]
        .as_array()
        .expect("callback input schema should be a tagged union");
    assert_eq!(variants.len(), 2);
    for variant in variants {
        assert_eq!(variant["type"].as_str(), Some("object"));
        let properties = variant["properties"]
            .as_object()
            .expect("callback input variant must expose object properties");
        assert!(
            properties.contains_key("op"),
            "callback input variants must include the op discriminator"
        );
        let op_schema = properties.get("op").expect("op property");
        assert_eq!(op_schema["type"].as_str(), Some("string"));
        assert!(
            op_schema.get("const").and_then(Value::as_str).is_some(),
            "op discriminator must pin a concrete variant value"
        );
        let required = variant["required"]
            .as_array()
            .expect("callback input variant must declare required fields");
        assert!(
            required.iter().any(|value| value.as_str() == Some("op")),
            "callback input variant must require the op discriminator"
        );
    }
}

#[tokio::test]
async fn callback_tool_schedules_and_dedupes_against_pending_rows() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    let first_output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "same-follow-up",
                "payload": { "kind": "reminder" }
            }),
        )
        .await,
    );
    assert_eq!(first_output["outcome"], "scheduled");
    assert_eq!(first_output["deduped"], false);
    let first_callback_id = first_output["callbackId"].as_str().unwrap().to_string();

    let second_output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "same-follow-up",
                "payload": { "kind": "reminder" }
            }),
        )
        .await,
    );
    assert_eq!(second_output["outcome"], "scheduled");
    assert_eq!(second_output["deduped"], true);
    assert_eq!(
        second_output["callbackId"].as_str(),
        Some(first_callback_id.as_str())
    );

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "pending");
    assert_eq!(
        rows[0].callback.dedupe_key.as_deref(),
        Some("same-follow-up")
    );
    assert_eq!(
        rows[0].callback.scheduling_context_id.as_ref(),
        Some(&context_id)
    );
    assert_eq!(rows[0].callback.scheduling_task_id.as_ref(), Some(&task_id));
    let expected_dispatch_ctx = first_output["dispatchContextId"]
        .as_str()
        .expect("detached default mints dispatchContextId");
    let expected_dispatch_task = first_output["dispatchTaskId"]
        .as_str()
        .expect("detached default mints dispatchTaskId");
    assert_eq!(
        rows[0].callback.context_id.as_ref().map(|c| c.as_str()),
        Some(expected_dispatch_ctx)
    );
    assert_eq!(
        rows[0].callback.task_id.as_ref().map(|t| t.as_str()),
        Some(expected_dispatch_task)
    );
    assert_eq!(
        rows[0].callback.requesting_agent_id.as_deref(),
        Some(agent_id.as_str())
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_resume_current_task_preserves_task_continuity() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    let output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "resume-current-task",
                "payload": { "kind": "resume" },
                "continuation": "resume_current_task"
            }),
        )
        .await,
    );
    assert_eq!(output["outcome"], "scheduled");

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].callback.context_id.as_ref(), Some(&context_id));
    assert_eq!(rows[0].callback.task_id.as_ref(), Some(&task_id));
    assert_eq!(
        rows[0].callback.scheduling_context_id.as_ref(),
        Some(&context_id)
    );
    assert_eq!(rows[0].callback.scheduling_task_id.as_ref(), Some(&task_id));

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_accepts_opaque_json_wrapper_payloads() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    let output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:follow-up",
                "payload": {
                    "__baml_opaque_json": "{\"kind\":\"wrapped\",\"count\":2}"
                }
            }),
        )
        .await,
    );
    assert_eq!(output["outcome"], "scheduled");

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].callback.payload,
        json!({
            "kind": "wrapped",
            "count": 2
        })
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_plan_from_baml_invocation_executes_wrapped_payloads() {
    let _suite_guard = suite_lock().lock().await;
    let _cleanup = CallbackGlobalsCleanup;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let schema_dir = write_callback_plan_schema_fixture();
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("create BAML runtime manager");
    manager
        .load_schema(schema_dir.path.to_str().expect("temp schema path"))
        .expect("load callback plan schema");
    manager
        .tool_registry()
        .register_bundle(SystemBundle::new(
            Arc::new(EmptyAgentList),
            manager.tool_registry(),
            Arc::new(MockA2aHandler),
        ))
        .expect("register system bundle");
    manager
        .register_llm_interceptor(StubCallbackPlanInterceptor)
        .await;

    let mut plan_map = HashMap::new();
    plan_map.insert(
        "PlanCallbackSchedule".to_string(),
        vec![SessionPlanTypeName::new("SystemCallbackSessionPlan").expect("plan type name")],
    );
    manager.set_session_plan_functions(Some(plan_map));

    let scope = InvocationScope::synthetic_task(test_agent_id());
    let invoke_result = context::with_scope(scope.as_scope().clone(), async {
        manager
            .invoke_function(
                scope.as_scope(),
                "PlanCallbackSchedule",
                json!({ "token": "wrapped" }),
            )
            .await
    })
    .await
    .expect("invoke callback planning function");
    assert_eq!(invoke_result["status"], "done");

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].callback.payload,
        json!({
            "kind": "wrapped",
            "count": 2
        })
    );
}

#[tokio::test]
async fn callback_tool_dedupe_is_scoped_per_source_key() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    let first = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:channel-a",
                "dedupeKey": "shared-key",
                "payload": { "from": "a" }
            }),
        )
        .await,
    );
    assert_eq!(first["deduped"], false);

    let second = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:channel-b",
                "dedupeKey": "shared-key",
                "payload": { "from": "b" }
            }),
        )
        .await,
    );
    assert_eq!(second["deduped"], false);
    assert_ne!(
        first["callbackId"].as_str(),
        second["callbackId"].as_str(),
        "same dedupeKey under different sourceKeys must create separate callbacks"
    );

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 2);

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_schedules_without_dedupe_and_waits_until_due() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();
    let output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 60_000,
                "sourceKey": "coordinator-agent:later",
                "payload": { "kind": "later" }
            }),
        )
        .await,
    );
    assert_eq!(output["outcome"], "scheduled");
    assert_eq!(output["deduped"], false);

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    assert_eq!(producers.len(), 1);

    let poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert!(poll.events.is_empty());
    assert!(poll.checkpoint.value().is_none());
    assert_eq!(
        store
            .snapshot()
            .await
            .into_iter()
            .filter(|row| row.status == "pending")
            .count(),
        1
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_cancels_by_callback_id() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    let scheduled = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 60_000,
                "sourceKey": "coordinator-agent:follow-up",
                "payload": { "kind": "reminder" }
            }),
        )
        .await,
    );
    let callback_id = scheduled["callbackId"].as_str().unwrap().to_string();

    let cancelled = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "cancel",
                "callbackId": callback_id
            }),
        )
        .await,
    );
    assert_eq!(cancelled["outcome"], "cancelled");
    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(
        cancelled["sourceKey"].as_str(),
        Some("coordinator-agent:follow-up")
    );

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "cancelled");

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_cancels_by_source_key_and_dedupe_key() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 60_000,
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "same-follow-up",
                "payload": { "kind": "reminder" }
            }),
        )
        .await,
    );

    let cancelled = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "cancel",
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "same-follow-up"
            }),
        )
        .await,
    );
    assert_eq!(cancelled["outcome"], "cancelled");
    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(cancelled["dedupeKey"].as_str(), Some("same-follow-up"));

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "cancelled");

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_cancel_validates_selector_combinations() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    install_callback_store(Arc::new(MemoryCallbackStore::default()));

    let registry = test_registry();
    let context_id = test_context_id();
    let agent_id = test_agent_id();
    let task_id = test_task_id();

    let cases = vec![
        (
            json!({ "op": "cancel" }),
            "system/callback cancel requires callbackId or sourceKey + dedupeKey",
        ),
        (
            json!({
                "op": "cancel",
                "sourceKey": "coordinator-agent:follow-up"
            }),
            "system/callback cancel requires dedupeKey when sourceKey is provided",
        ),
        (
            json!({
                "op": "cancel",
                "dedupeKey": "same-follow-up"
            }),
            "system/callback cancel requires sourceKey when dedupeKey is provided",
        ),
        (
            json!({
                "op": "cancel",
                "callbackId": "cb-1",
                "sourceKey": "coordinator-agent:follow-up"
            }),
            "system/callback cancel accepts either callbackId or sourceKey + dedupeKey, not both",
        ),
        (
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "coordinator-agent:follow-up",
                "continuation": "resume_current_task",
                "payload": { "kind": "resume" }
            }),
            "system/callback continuation=resume_current_task requires dedupeKey",
        ),
    ];

    for (input, expected_message) in cases {
        let step = invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            input,
        )
        .await;
        assert!(
            expect_error_message(step).ends_with(expected_message),
            "expected error to end with '{expected_message}'"
        );
    }

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_schedule_requires_active_task_scope() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    install_callback_store(Arc::new(MemoryCallbackStore::default()));

    let registry = test_registry();
    let context_id = test_context_id();
    let agent_id = test_agent_id();

    let step = invoke_callback_tool(
        registry.as_ref(),
        &context_id,
        &agent_id,
        None,
        json!({
            "op": "schedule",
            "afterMs": 250,
            "sourceKey": "coordinator-agent:follow-up",
            "dedupeKey": "resume-current-task",
            "continuation": "resume_current_task",
            "payload": { "kind": "resume" }
        }),
    )
    .await;
    assert!(
        expect_error_message(step).ends_with(
            "system/callback schedule requires an active task scope (scheduling deferral)"
        )
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_cancel_returns_false_for_nonexistent_id() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    install_callback_store(Arc::new(MemoryCallbackStore::default()));

    let registry = test_registry();
    let context_id = test_context_id();
    let agent_id = test_agent_id();
    let task_id = test_task_id();

    let output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "cancel",
                "callbackId": "cb-does-not-exist"
            }),
        )
        .await,
    );
    assert_eq!(output["outcome"], "cancelled");
    assert_eq!(output["cancelled"], false);

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_cancel_returns_false_for_already_delivered() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let agent_id = test_agent_id();
    let task_id = test_task_id();

    let scheduled = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 0,
                "sourceKey": "coordinator-agent:follow-up",
                "payload": { "kind": "reminder" }
            }),
        )
        .await,
    );
    let callback_id = scheduled["callbackId"].as_str().unwrap().to_string();

    store
        .mark_callbacks_delivered(std::slice::from_ref(&callback_id), 999)
        .await
        .unwrap();

    let output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "cancel",
                "callbackId": callback_id
            }),
        )
        .await,
    );
    assert_eq!(output["outcome"], "cancelled");
    assert_eq!(output["cancelled"], false);

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_reschedule_ignores_emitted_pending_rows() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let registry = test_registry();
    let context_id = test_context_id();
    let agent_id = test_agent_id();
    let task_id = test_task_id();

    let first_output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 0,
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "follow-up-once",
                "payload": { "kind": "reminder" }
            }),
        )
        .await,
    );
    let first_callback_id = first_output["callbackId"].as_str().unwrap().to_string();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    let first_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(first_poll.events.len(), 1);

    let second_output = expect_done_output(
        invoke_callback_tool(
            registry.as_ref(),
            &context_id,
            &agent_id,
            Some(&task_id),
            json!({
                "op": "schedule",
                "afterMs": 0,
                "sourceKey": "coordinator-agent:follow-up",
                "dedupeKey": "follow-up-once",
                "payload": { "kind": "reminder-again" }
            }),
        )
        .await,
    );
    assert_eq!(second_output["outcome"], "scheduled");
    assert_eq!(second_output["deduped"], false);
    assert_ne!(
        second_output["callbackId"].as_str(),
        Some(first_callback_id.as_str())
    );

    let rows = store.snapshot().await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter(|row| row.status == "pending" && row.emitted_at_unix_ms.is_some())
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.status == "pending" && row.emitted_at_unix_ms.is_none())
            .count(),
        1
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_tool_rejects_empty_source_key() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    install_callback_store(Arc::new(MemoryCallbackStore::default()));

    let registry = test_registry();
    let context_id = test_context_id();
    let agent_id = test_agent_id();
    let task_id = test_task_id();

    let step = invoke_callback_tool(
        registry.as_ref(),
        &context_id,
        &agent_id,
        Some(&task_id),
        json!({
            "op": "schedule",
            "afterMs": 100,
            "sourceKey": "",
            "payload": { "kind": "test" }
        }),
    )
    .await;
    let message = expect_error_message(step);
    assert!(
        message.contains("invalid format"),
        "expected sourceKey validation error, got: {message}"
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_producer_returns_empty_when_store_is_not_installed() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();

    assert!(producers.is_empty());
}

#[tokio::test]
async fn callback_producer_errors_on_invalid_stored_source_key() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    clear_callback_delivery_gate();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    store
        .schedule_callback(ScheduleCallbackRequest {
            source_key: String::new(),
            dedupe_key: None,
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: None,
            task_id: None,
            scheduling_context_id: Some(test_context_id()),
            scheduling_task_id: Some(test_task_id()),
            requesting_agent_id: None,
            requesting_message_id: None,
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    assert_eq!(producers.len(), 1);

    let err = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap_err();
    match err {
        BamlRtError::InvalidArgument(message) => {
            assert!(message.contains("system/callback stored invalid source key"));
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }

    clear_callback_store();
    clear_callback_delivery_gate();
}

#[tokio::test]
async fn callback_producer_defers_until_delivery_gate_opens() {
    let _suite_guard = suite_lock().lock().await;
    let _cleanup = CallbackGlobalsCleanup;
    clear_callback_store();
    clear_callback_delivery_gate();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());
    let allow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    install_callback_delivery_gate(Arc::new(ToggleDeliveryGate {
        allow: allow.clone(),
    }));

    let scheduled = store
        .schedule_callback(ScheduleCallbackRequest {
            source_key: "coordinator-agent:resume".to_string(),
            dedupe_key: Some("resume-now".to_string()),
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: Some(test_context_id()),
            task_id: Some(test_task_id()),
            scheduling_context_id: Some(test_context_id()),
            scheduling_task_id: Some(test_task_id()),
            requesting_agent_id: Some(test_agent_id().as_str().to_string()),
            requesting_message_id: None,
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    assert_eq!(producers.len(), 1);

    let first_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert!(
        first_poll.events.is_empty(),
        "callback should stay deferred"
    );
    assert!(
        first_poll.checkpoint.value().is_none(),
        "deferral should not advance checkpoint"
    );
    let rows_after_deferral = store.snapshot().await;
    assert_eq!(rows_after_deferral.len(), 1);
    assert_eq!(rows_after_deferral[0].status, "pending");
    assert!(
        rows_after_deferral[0].emitted_at_unix_ms.is_none(),
        "deferred callback must remain unemitted"
    );

    allow.store(true, Ordering::Relaxed);
    let second_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(second_poll.events.len(), 1);
    assert_eq!(
        second_poll.events[0].messages[0]["callback_id"].as_str(),
        Some(scheduled.callback.callback_id.as_str())
    );
}

#[tokio::test]
async fn callback_producer_polls_and_reconciles_delivery() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let scheduled = store
        .schedule_callback(ScheduleCallbackRequest {
            source_key: "coordinator-agent:resume".to_string(),
            dedupe_key: Some("resume-1".to_string()),
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: Some(ContextId::new(50, 4)),
            task_id: Some(TaskId::from_external(ExternalId::new("task-resume-1"))),
            scheduling_context_id: Some(ContextId::new(50, 4)),
            scheduling_task_id: Some(TaskId::from_external(ExternalId::new("task-resume-1"))),
            requesting_agent_id: Some("agent-123".to_string()),
            requesting_message_id: Some(MessageId::from("msg-123")),
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    assert_eq!(producers.len(), 1);

    let first_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(first_poll.events.len(), 1);
    assert_eq!(
        first_poll.events[0].schema_version.as_str(),
        CALLBACK_EVENT_SCHEMA_VERSION
    );
    assert_eq!(
        first_poll.events[0].source_kind.as_str(),
        CALLBACK_SOURCE_KIND
    );
    assert_eq!(
        first_poll.events[0].source_key.as_str(),
        "coordinator-agent:resume"
    );
    // Routing fields the event dispatcher needs to deliver to the right agent/task.
    assert_eq!(
        first_poll.events[0].routing_key.as_str(),
        CALLBACK_EVENT_ROUTING_KEY
    );
    assert_eq!(
        first_poll.events[0].context_id.as_ref(),
        Some(&ContextId::new(50, 4)),
        "context_id must flow through from the schedule request for dispatch routing"
    );
    assert_eq!(
        first_poll.events[0].task_id.as_ref().map(|id| id.as_str()),
        Some("task-resume-1"),
        "task_id must flow through from the schedule request for dispatch routing"
    );
    assert_eq!(
        first_poll.events[0].message_id.as_deref(),
        Some(
            format!(
                "system/callback:{callback_id}",
                callback_id = scheduled.callback.callback_id
            )
            .as_str()
        ),
        "message_id must be derived from callback_id for idempotent delivery"
    );
    assert_eq!(
        first_poll.events[0].messages[0]["callback_id"].as_str(),
        Some(scheduled.callback.callback_id.as_str())
    );
    assert_eq!(
        first_poll.events[0].messages[0]["payload"],
        json!({"goal": "resume"})
    );
    let meta = first_poll.events[0]
        .metadata
        .as_ref()
        .expect("callback producer attaches scheduling metadata");
    assert_eq!(
        meta[DISPATCH_METADATA_SCHEDULING_CONTEXT_ID].as_str(),
        Some(ContextId::new(50, 4).as_str())
    );
    assert_eq!(
        meta[DISPATCH_METADATA_SCHEDULING_TASK_ID].as_str(),
        Some("task-resume-1")
    );
    assert!(first_poll.checkpoint.value().is_some());
    assert_eq!(
        serde_json::from_str::<Value>(first_poll.checkpoint.value().unwrap()).unwrap(),
        json!({
            "delivered_callback_ids": [scheduled.callback.callback_id]
        })
    );
    assert_eq!(
        store
            .snapshot()
            .await
            .into_iter()
            .filter(|row| row.status == "pending" && row.emitted_at_unix_ms.is_some())
            .count(),
        1
    );

    let second_poll = producers[0].poll(&first_poll.checkpoint).await.unwrap();
    assert!(second_poll.events.is_empty());
    assert_eq!(
        serde_json::from_str::<Value>(second_poll.checkpoint.value().unwrap()).unwrap(),
        json!({
            "delivered_callback_ids": []
        })
    );
    assert_eq!(
        store
            .snapshot()
            .await
            .into_iter()
            .filter(|row| row.status == "delivered")
            .count(),
        1
    );

    let third_poll = producers[0].poll(&second_poll.checkpoint).await.unwrap();
    assert!(third_poll.events.is_empty());
    assert!(third_poll.checkpoint.value().is_none());

    clear_callback_store();
}

#[tokio::test]
async fn callback_producer_reconciles_after_simulated_restart() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let scheduled = store
        .schedule_callback(ScheduleCallbackRequest {
            source_key: "coordinator-agent:resume".to_string(),
            dedupe_key: None,
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: Some(ContextId::new(50, 4)),
            task_id: None,
            scheduling_context_id: Some(ContextId::new(50, 4)),
            scheduling_task_id: Some(TaskId::from_external(ExternalId::new("sched-restart-1"))),
            requesting_agent_id: None,
            requesting_message_id: None,
        })
        .await
        .unwrap();

    // First producer polls and returns events + checkpoint.
    let first_producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    let first_poll = first_producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(first_poll.events.len(), 1);
    assert_eq!(
        first_poll.events[0].messages[0]["callback_id"].as_str(),
        Some(scheduled.callback.callback_id.as_str())
    );

    // Simulate crash: checkpoint was persisted but mark_callbacks_delivered
    // never ran because the host died before the next poll. The callback is
    // still "pending" in the store.
    assert_eq!(
        store
            .snapshot()
            .await
            .into_iter()
            .filter(|row| row.status == "pending")
            .count(),
        1
    );

    // Build a fresh producer (simulating process restart) and pass the
    // persisted checkpoint from the prior run to the first poll.
    let restart_producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();
    let restart_poll = restart_producers[0]
        .poll(&first_poll.checkpoint)
        .await
        .unwrap();

    // The reconciliation path should have marked the callback delivered,
    // and since there are no new due callbacks the poll returns empty.
    assert!(
        restart_poll.events.is_empty(),
        "no new events expected after reconciliation"
    );
    assert_eq!(
        store
            .snapshot()
            .await
            .into_iter()
            .filter(|row| row.status == "delivered")
            .count(),
        1,
        "reconciliation should have marked the callback delivered"
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_producer_redelivers_pending_rows_when_checkpoint_is_missing() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let scheduled = store
        .schedule_callback(ScheduleCallbackRequest {
            source_key: "coordinator-agent:resume".to_string(),
            dedupe_key: Some("resume-1".to_string()),
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: None,
            task_id: None,
            scheduling_context_id: Some(test_context_id()),
            scheduling_task_id: Some(test_task_id()),
            requesting_agent_id: Some("agent-123".to_string()),
            requesting_message_id: None,
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();

    let first_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(first_poll.events.len(), 1);
    assert_eq!(
        first_poll.events[0].messages[0]["callback_id"].as_str(),
        Some(scheduled.callback.callback_id.as_str())
    );

    let second_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(second_poll.events.len(), 1);
    assert_eq!(
        second_poll.events[0].messages[0]["callback_id"].as_str(),
        Some(scheduled.callback.callback_id.as_str()),
        "pending emitted rows must be redelivered when the host never persisted a checkpoint"
    );

    clear_callback_store();
}

#[tokio::test]
async fn callback_producer_respects_max_poll_limit() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    // Schedule 105 immediately-due callbacks (exceeds MAX_CALLBACKS_PER_POLL = 100).
    for i in 0..105 {
        store
            .schedule_callback(ScheduleCallbackRequest {
                source_key: "coordinator-agent:bulk".to_string(),
                dedupe_key: None,
                payload: json!({ "index": i }),
                scheduled_for_unix_ms: 0,
                requested_at_unix_ms: 0,
                context_id: None,
                task_id: None,
                scheduling_context_id: Some(ContextId::new(0, i)),
                scheduling_task_id: Some(TaskId::from_external(ExternalId::new(format!(
                    "bulk-sched-{i}"
                )))),
                requesting_agent_id: None,
                requesting_message_id: None,
            })
            .await
            .unwrap();
    }

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
        ingress_store: None,
    })
    .await
    .unwrap();

    let first_poll = producers[0]
        .poll(&ProducerCheckpoint::none())
        .await
        .unwrap();
    assert_eq!(
        first_poll.events.len(),
        100,
        "first poll should return at most MAX_CALLBACKS_PER_POLL events"
    );

    // Mark the first batch delivered, then poll again for the remainder.
    let second_poll = producers[0].poll(&first_poll.checkpoint).await.unwrap();
    assert_eq!(
        second_poll.events.len(),
        5,
        "second poll should return the remaining 5 callbacks"
    );

    clear_callback_store();
}
