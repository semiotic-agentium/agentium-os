#![recursion_limit = "256"]

use baml_rt::a2a_types::{JSONRPCId, JSONRPCRequest};
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig};
use baml_rt_core::context;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use test_support::common::{
    AddNumbersTool, CalculatorTool, first_message_text_from_stream, first_task_id_from_stream,
    send_stream_request,
};

async fn collect_responses(
    agent: &A2aAgent,
    request: serde_json::Value,
) -> baml_rt::Result<Vec<Value>> {
    Ok(baml_rt_core::collect_a2a_stream(agent.handle_a2a_stream(request).await?).await)
}

fn fixture_js_code() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";

        if (text.startsWith("long-rite:")) {
            // First status must be SUBMITTED per FSM; then WORKING so subscribe sees status updates
            __chat_yield({
                task: {
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_SUBMITTED" }
                }
            });
            __chat_yield({
                statusUpdate: { status: { state: "TASK_STATE_WORKING" } }
            });
            return;
        }
        if (text.startsWith("tool-call:")) {
            try {
                const session = await openToolSession("test/add_numbers");
                await session.send({ a: 2, b: 3 });
                await session.continue();
                __chat_yield({ message: { parts: [{ text: "sum=5" }] } });
            } catch (e) {
                __chat_yield({ message: { parts: [{ text: `tool_error=${String(e)}` }] } });
            }
            return;
        }
        if (text.startsWith("baml-tool:")) {
            try {
                const session = await openToolSession("support/calculate");
                await session.send({ expression: { left: 2, operation: "Add", right: 3 } });
                await session.continue();
                __chat_yield({ message: { parts: [{ text: "sum=5" }] } });
            } catch (e) {
                __chat_yield({ message: { parts: [{ text: `tool_error=${String(e)}` }] } });
            }
            return;
        }
        __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
        __chat_yield({ artifactUpdate: { artifact: { name: "rite-log", parts: [{ text: "sealed" }] } } });
    };
    "#
    .to_string()
}

async fn acquire_test_permit() -> tokio::sync::OwnedSemaphorePermit {
    static TEST_GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    TEST_GATE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("test gate permit")
}

async fn setup_agent() -> A2aAgent {
    let manager = BamlRuntimeManager::new().unwrap();
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(fixture_js_code())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap()
}

const SYSTEM_A2A_TOOL: &str = "system/internal_a2a";

/// Agent with system/internal_a2a tool registered on the given LocalSet (for session FSM tests).
/// Caller must run agent work inside `local_set.run_until(...)` so the session worker is driven.
async fn setup_agent_with_a2a_session_tool() -> (A2aAgent, tokio::task::LocalSet) {
    let local_set = tokio::task::LocalSet::new();
    let manager = BamlRuntimeManager::new().unwrap();
    let agent = A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(fixture_js_code())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_a2a_session_tool(true)
        .build()
        .await
        .unwrap();
    (agent, local_set)
}

#[tokio::test]
async fn test_message_send_deterministic_task() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    let request = send_stream_request(
        "vox-1",
        "long-rite: reactor benediction",
        "corr-3-1",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );

    let responses = collect_responses(&agent, request).await.unwrap();
    let result = responses[0].get("result").cloned().unwrap_or(Value::Null);
    let content = result.get("chunk").cloned().unwrap_or(result);
    let task_id = content
        .get("task")
        .and_then(|task| task.get("id"))
        .and_then(|value| value.as_str());
    assert!(
        task_id.is_some_and(|id| id.starts_with("js-task-")),
        "expected generated js-task-* id, got {:?}",
        task_id
    );
}

#[tokio::test]
async fn test_message_send_stream_emits_updates() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    let request = send_stream_request(
        "vox-2",
        "ignite the void seals",
        "corr-3-2",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );

    let responses = collect_responses(&agent, request).await.unwrap();

    let mut saw_status = false;
    let mut saw_artifact = false;
    for response in responses {
        if let Some(chunk) = response
            .get("result")
            .and_then(|result| result.get("chunk"))
        {
            if chunk.get("statusUpdate").is_some() {
                saw_status = true;
            }
            if chunk.get("artifactUpdate").is_some() {
                saw_artifact = true;
            }
        }
    }

    assert!(saw_status, "expected a statusUpdate stream chunk");
    assert!(saw_artifact, "expected an artifactUpdate stream chunk");
}

#[tokio::test]
async fn test_tasks_subscribe_streams_incremental_updates() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    let create_request = send_stream_request(
        "vox-3",
        "long-rite: plasma canticle",
        "corr-3-3",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );
    let created = collect_responses(&agent, serde_json::to_value(create_request).unwrap())
        .await
        .unwrap();
    let task_id = first_task_id_from_stream(&created).expect("task id from create stream");

    let stream_request = send_stream_request(
        "vox-3",
        "ignite the void seals",
        "corr-3-4",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );
    let _ = collect_responses(&agent, serde_json::to_value(stream_request).unwrap())
        .await
        .unwrap();

    let subscribe_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks.subscribe".to_string(),
        params: Some(json!({ "id": task_id, "stream": true })),
        id: Some(JSONRPCId::String("corr-3-5".to_string())),
    };
    let responses = collect_responses(&agent, serde_json::to_value(subscribe_request).unwrap())
        .await
        .unwrap();

    let mut saw_status = false;
    let mut saw_artifact = false;
    for response in responses {
        if let Some(chunk) = response
            .get("result")
            .and_then(|result| result.get("chunk"))
        {
            if chunk.get("statusUpdate").is_some() {
                saw_status = true;
            }
            if chunk.get("artifactUpdate").is_some() {
                saw_artifact = true;
            }
        }
    }

    assert!(saw_status, "expected status updates in subscribe stream");
    assert!(
        saw_artifact || saw_status,
        "expected at least status or artifact updates in subscribe stream"
    );
}

#[tokio::test]
async fn test_message_send_tool_calling() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    {
        let runtime = agent.runtime();
        let mut manager = runtime.lock().await;
        manager.register_tool(AddNumbersTool).await.unwrap();
    }

    let request = send_stream_request(
        "vox-4",
        "tool-call: add numbers",
        "corr-3-6",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );

    let responses = collect_responses(&agent, request).await.unwrap();
    let text = first_message_text_from_stream(&responses);
    assert!(
        text.contains("sum=5"),
        "expected tool result in message text, got: {}",
        text
    );
}

#[tokio::test]
async fn test_message_send_baml_tool_calling() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    {
        let runtime = agent.runtime();
        let mut manager = runtime.lock().await;
        manager.register_tool(CalculatorTool).await.unwrap();
    }

    let request = send_stream_request(
        "vox-5",
        "baml-tool: rite of sums",
        "corr-3-7",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );

    let responses = collect_responses(&agent, request).await.unwrap();
    let text = first_message_text_from_stream(&responses);
    assert!(
        text.contains("sum=5"),
        "expected BAML tool result in message text, got: {}",
        text
    );
}

/// Session send() only enqueues; must return in under 50ms. next() drains until Done.
#[tokio::test(flavor = "current_thread")]
async fn test_a2a_session_send_returns_fast_and_next_drains() {
    let _permit = acquire_test_permit().await;
    let (agent, local_set) = setup_agent_with_a2a_session_tool().await;
    let handle = {
        let runtime = agent.runtime();
        let mgr = runtime.lock().await;
        mgr.tool_session_handle()
    };
    local_set
        .run_until(async move {
            let context_id = context::generate_context_id();
            let message_id = baml_rt_core::ids::MessageId::from_external(
                baml_rt_core::ids::ExternalId::new(format!("msg-{}", context_id.as_str())),
            );
            let scope = context::RuntimeScope::message_scope(
                context_id,
                agent.agent_id().clone(),
                message_id,
            );
            let scope_for_open = scope.clone();
            context::with_scope(scope, async move {
                let session_id = handle
                    .open_tool_session(
                        &scope_for_open,
                        SYSTEM_A2A_TOOL,
                        json!({ "target": { "agent_package": "self", "agent_instance_id": "default" } }),
                    )
                    .await
                    .expect("open system/internal_a2a");
                let send_input = json!({ "text": "ping" });
                let start = std::time::Instant::now();
                handle
                    .tool_session_send(&session_id, send_input.clone())
                    .await
                    .expect("session_send");
                let elapsed = start.elapsed();
                assert!(
                    elapsed < std::time::Duration::from_millis(50),
                    "session send() must return in under 50ms (enqueue only), took {:?}",
                    elapsed
                );
                for _ in 0..8 {
                    let next = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        handle.tool_session_next(&session_id),
                    )
                    .await;
                    let Ok(Ok(step)) = next else {
                        break;
                    };
                    if matches!(
                        step,
                        baml_rt_tools::ToolStep::Done { .. }
                            | baml_rt_tools::ToolStep::Error { .. }
                    ) {
                        break;
                    }
                }
                handle
                    .tool_session_finish(&session_id)
                    .await
                    .expect("session_finish");
            })
            .await
        })
        .await;
}

/// send() after finish() must fail fast (terminal phase).
#[tokio::test(flavor = "current_thread")]
async fn test_a2a_session_send_after_finish_fails() {
    let _permit = acquire_test_permit().await;
    let (agent, local_set) = setup_agent_with_a2a_session_tool().await;
    let handle = {
        let runtime = agent.runtime();
        let mgr = runtime.lock().await;
        mgr.tool_session_handle()
    };
    local_set
        .run_until(async move {
            let context_id = context::generate_context_id();
            let message_id = baml_rt_core::ids::MessageId::from_external(
                baml_rt_core::ids::ExternalId::new(format!("msg-{}", context_id.as_str())),
            );
            let scope = context::RuntimeScope::message_scope(
                context_id,
                agent.agent_id().clone(),
                message_id,
            );
            let scope_for_open = scope.clone();
            context::with_scope(scope, async move {
                let session_id = handle
                    .open_tool_session(
                        &scope_for_open,
                        SYSTEM_A2A_TOOL,
                        json!({ "target": { "agent_package": "self", "agent_instance_id": "default" } }),
                    )
                    .await
                    .expect("open system/internal_a2a");
                let send_input = json!({ "text": "hi" });
                handle
                    .tool_session_send(&session_id, send_input.clone())
                    .await
                    .expect("first send");
                for _ in 0..8 {
                    let next = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        handle.tool_session_next(&session_id),
                    )
                    .await;
                    let Ok(Ok(step)) = next else {
                        break;
                    };
                    if matches!(
                        step,
                        baml_rt_tools::ToolStep::Done { .. }
                            | baml_rt_tools::ToolStep::Error { .. }
                    ) {
                        break;
                    }
                }
                handle
                    .tool_session_finish(&session_id)
                    .await
                    .expect("finish");
                let err = handle
                    .tool_session_send(&session_id, send_input)
                    .await
                    .expect_err("send after finish must fail");
                let err_msg = err.to_string();
                assert!(
                    err_msg.contains("terminal")
                        || err_msg.contains("closed")
                        || err_msg.contains("Unknown tool session")
                        || err_msg.contains("Unknown session"),
                    "error should mention terminal/closed/unknown-session: {}",
                    err_msg
                );
            })
            .await
        })
        .await;
}
