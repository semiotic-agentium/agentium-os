use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use baml_derive_core::JsonSchemaType;
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentLister,
    BamlRtError, BusStream, ContextId, Result,
    ids::{AgentId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_tools::{EventProducerBuildContext, ProducerCheckpoint, ToolRegistry, ToolStep};
use baml_tools_system::{
    CallbackToolInput, SystemBundle,
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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:follow-up",
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
    assert_eq!(rows[0].callback.context_id, None);
    assert_eq!(rows[0].callback.task_id, None);
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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:channel-a",
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
                "sourceKey": "workflow-intake:channel-b",
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
                "sourceKey": "workflow-intake:later",
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
                "sourceKey": "workflow-intake:follow-up",
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
        Some("workflow-intake:follow-up")
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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:follow-up"
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
                "sourceKey": "workflow-intake:follow-up"
            }),
            "system/callback cancel accepts either callbackId or sourceKey + dedupeKey, not both",
        ),
        (
            json!({
                "op": "schedule",
                "afterMs": 250,
                "sourceKey": "workflow-intake:follow-up",
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
async fn callback_tool_resume_current_task_requires_active_task_scope() {
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
            "sourceKey": "workflow-intake:follow-up",
            "dedupeKey": "resume-current-task",
            "continuation": "resume_current_task",
            "payload": { "kind": "resume" }
        }),
    )
    .await;
    assert!(expect_error_message(step).ends_with(
        "system/callback continuation=resume_current_task requires an active task scope"
    ));

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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:follow-up",
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
                "sourceKey": "workflow-intake:follow-up",
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
    })
    .await
    .unwrap();

    assert!(producers.is_empty());
}

#[tokio::test]
async fn callback_producer_errors_on_invalid_stored_source_key() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
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
            requesting_agent_id: None,
            requesting_message_id: None,
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
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
}

#[tokio::test]
async fn callback_producer_polls_and_reconciles_delivery() {
    let _suite_guard = suite_lock().lock().await;
    clear_callback_store();
    let store = Arc::new(MemoryCallbackStore::default());
    install_callback_store(store.clone());

    let scheduled = store
        .schedule_callback(ScheduleCallbackRequest {
            source_key: "workflow-intake:resume".to_string(),
            dedupe_key: Some("resume-1".to_string()),
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: Some(ContextId::new(50, 4)),
            task_id: Some(TaskId::from_external(ExternalId::new("task-resume-1"))),
            requesting_agent_id: Some("agent-123".to_string()),
            requesting_message_id: Some(MessageId::from("msg-123")),
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
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
        "workflow-intake:resume"
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
            source_key: "workflow-intake:resume".to_string(),
            dedupe_key: None,
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: Some(ContextId::new(50, 4)),
            task_id: None,
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
            source_key: "workflow-intake:resume".to_string(),
            dedupe_key: Some("resume-1".to_string()),
            payload: json!({"goal": "resume"}),
            scheduled_for_unix_ms: 0,
            requested_at_unix_ms: 0,
            context_id: None,
            task_id: None,
            requesting_agent_id: Some("agent-123".to_string()),
            requesting_message_id: None,
        })
        .await
        .unwrap();

    let producers = build_callback_event_producers(EventProducerBuildContext {
        metadata: system_callback_metadata(),
        config: None,
        persisted_checkpoints: Arc::new(HashMap::new()),
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
                source_key: "workflow-intake:bulk".to_string(),
                dedupe_key: None,
                payload: json!({ "index": i }),
                scheduled_for_unix_ms: 0,
                requested_at_unix_ms: 0,
                context_id: None,
                task_id: None,
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
