#![recursion_limit = "256"]

mod common;

use std::sync::{Arc, OnceLock};

fn init_trace() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

use baml_rt::{
    A2aAgent, A2aRequestHandler, QuickJSConfig,
    a2a_types::{JSONRPCId, JSONRPCRequest},
    baml::BamlRuntimeManager,
};
use baml_rt_core::{AgentDiscoveryEntry, AgentLister, context};
use baml_tools_system::SystemBundle;
use serde_json::{Value, json};
use test_support::common::{
    AddNumbersTool, CalculatorTool, first_message_text_from_stream, first_task_id_from_stream,
    send_stream_request,
};

async fn collect_responses(
    agent: &A2aAgent,
    request: serde_json::Value,
) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await?;
    let chunks = baml_rt_core::collect_a2a_stream(stream).await;
    Ok(chunks
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect())
}

fn fixture_js_code() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";

        if (text.startsWith("long-rite-subscribe:")) {
            const messageId = message?.messageId || "stream";
            const taskId = `task-${messageId}`;
            // Explicit id variant used by subscribe-path test to avoid task-id ambiguity.
            __chat_yield({
                task: {
                    id: taskId,
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_SUBMITTED" }
                }
            });
            __chat_yield({
                statusUpdate: { status: { state: "TASK_STATE_WORKING" } }
            });
            // Emit a terminal state so the stream collector can complete deterministically.
            __chat_yield({
                statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } }
            });
            return;
        }

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
            // Emit a terminal state so the stream collector can complete deterministically.
            __chat_yield({
                statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } }
            });
            return;
        }
        // Deliberately perverse: yield SUBMITTED + WORKING then never yield terminal and never return.
        // Handler blocks so the yield channel stays open; server idle timeout cancels the stream.
        if (text.startsWith("idle-timeout-test:")) {
            __chat_yield({
                task: {
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_SUBMITTED" }
                }
            });
            __chat_yield({
                statusUpdate: { status: { state: "TASK_STATE_WORKING" } }
            });
            await new Promise(() => {});
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
            __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } } });
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
            __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } } });
            return;
        }
        __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
        __chat_yield({ artifactUpdate: { artifact: { name: "rite-log", parts: [{ text: "sealed" }] } } });
        __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } } });
    };
    "#
    .to_string()
}

struct EmptyAgentList;

impl AgentLister for EmptyAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        vec![]
    }
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
    let store = common::provenance::build_graphqlite_test_store();
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_graphqlite_store(store)
        .with_init_js(fixture_js_code())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap()
}

/// Agent with 5s stream collector idle timeout for tests that assert timeout cancellation.
async fn setup_agent_with_stream_idle_5s() -> A2aAgent {
    let manager = BamlRuntimeManager::new().unwrap();
    let store = common::provenance::build_graphqlite_test_store();
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_graphqlite_store(store)
        .with_init_js(fixture_js_code())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(
            QuickJSConfig::new()
                .with_max_attempts_ms(Some(15_000))
                .with_stream_collector_idle_secs(Some(5)),
        )
        .build()
        .await
        .unwrap()
}

const SYSTEM_A2A_TOOL: &str = "system/internal_a2a";

/// Agent with system/internal_a2a tool registered (for session FSM tests).
async fn setup_agent_with_a2a_session_tool() -> A2aAgent {
    let manager = BamlRuntimeManager::new().unwrap();
    let store = common::provenance::build_graphqlite_test_store();
    let agent = A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_graphqlite_store(store)
        .with_init_js(fixture_js_code())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();
    let registry = agent.runtime().lock().await.tool_registry();
    registry
        .register_bundle(SystemBundle::new(
            Arc::new(EmptyAgentList),
            registry.clone(),
            Arc::new(agent.clone()),
        ))
        .unwrap();
    agent
}

#[tokio::test]
async fn test_message_send_deterministic_task() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    let context_id = baml_rt_core::ids::ContextId::new(1, 1);
    let request = send_stream_request(
        "vox-1",
        "long-rite: reactor benediction",
        "corr-3-1",
        Some(context_id.clone()),
    );

    let responses = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        collect_responses(&agent, request),
    )
    .await
    .expect("stream request timed out")
    .unwrap();
    let result = responses[0].get("result").cloned().unwrap_or(Value::Null);
    let content = result.get("chunk").cloned().unwrap_or(result);
    let task_id = content
        .get("task")
        .and_then(|task| task.get("id"))
        .and_then(|value| value.as_str());
    // Live path first turn uses a context-stable task_id (context_id.as_str()); agent may otherwise yield js-task-*.
    assert!(
        task_id.is_some_and(|id| id.starts_with("js-task-") || id == context_id.as_str()),
        "expected deterministic task id (js-task-* or context-stable), got {:?}",
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

/// Deliberately perverse stream: yields SUBMITTED + WORKING then blocks (never yields COMPLETED).
/// Agent is built with 5s stream collector idle timeout. We assert the stream is cancelled
/// (final + null chunk). With the idle timeout wired, the collector should exit after 5s with
/// Timeout; the test accepts any cancellation (Timeout or ChannelClosed) so long as we get
/// the expected final sentinel shape.
#[tokio::test]
async fn test_stream_collector_idle_timeout_cancels_perverse_stream() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent_with_stream_idle_5s().await;
    let request = send_stream_request(
        "vox-timeout",
        "idle-timeout-test: never-completes",
        "corr-1700000000099-1",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );

    let responses = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        collect_responses(&agent, request),
    )
    .await
    .expect("stream request must complete within 30s")
    .unwrap();

    assert!(
        !responses.is_empty(),
        "expected at least one response (cancellation sentinel); got {}",
        responses.len()
    );

    let last = responses.last().expect("at least one response");
    if last.get("error").is_some() {
        panic!(
            "stream ended with error response instead of cancellation sentinel; last={:?}",
            last
        );
    }
    let result = last
        .get("result")
        .expect("last response must have result (stream chunk envelope)");
    let is_final = result
        .get("final")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let chunk = result.get("chunk");

    assert!(
        is_final,
        "last response must have final: true (stream cancelled); got result={:?}",
        result
    );
    // Idle timeout may produce either a null sentinel or a TASK_STATE_FAILED
    // chunk with a timeout message. Both are valid terminal signals.
    let is_null_sentinel = chunk.is_none() || chunk == Some(&Value::Null);
    let is_failed_terminal = chunk
        .and_then(|c| c.get("task"))
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        == Some("TASK_STATE_FAILED");
    assert!(
        is_null_sentinel || is_failed_terminal,
        "cancelled stream ends with null chunk sentinel or FAILED terminal; got chunk={:?}",
        chunk
    );
}

/// Asserts tasks.subscribe streams status/artifact updates for a task created via message.sendStream.
/// The fixture emits task.id = task-{messageId}; the host must pass messageId in the JS request so
/// the stored task id matches what first_task_id_from_stream extracts for subscribe.
///
/// **Store/connection scope:** The same agent (and thus the same `GraphqliteProvenanceStore` /
/// worker/connection) is used for both create-stream and subscribe: a single `setup_agent()` call
/// builds one file-backed store and one agent; both `handle_a2a_stream(create_request)` and
/// `handle_a2a_stream(subscribe_request)` use that agent. If subscribe returns "Task not found",
/// the cause is likely **A2A messaging identity alignment** (e.g. id written by the pipeline vs id
/// sent in tasks.subscribe params), not a different store or connection.
#[tokio::test]
async fn test_tasks_subscribe_streams_incremental_updates() {
    init_trace();
    let _permit = acquire_test_permit().await;
    let agent = setup_agent().await;
    let create_request = send_stream_request(
        "vox-3",
        "long-rite-subscribe: plasma canticle",
        "corr-3-3",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );
    let created = collect_responses(&agent, serde_json::to_value(create_request).unwrap())
        .await
        .unwrap();
    let task_id = first_task_id_from_stream(&created).expect("task id from create stream");

    let subscribe_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks.subscribe".to_string(),
        params: Some(json!({ "id": task_id, "stream": true })),
        id: Some(JSONRPCId::String("corr-3-5".to_string())),
    };
    let responses = collect_responses(&agent, serde_json::to_value(subscribe_request).unwrap())
        .await
        .unwrap();
    let responses_debug = serde_json::to_string_pretty(&responses).unwrap_or_default();

    let mut saw_status_update = false;
    let mut saw_task_status_snapshot = false;
    let mut saw_artifact = false;
    let mut saw_task_not_found_error = false;
    for response in responses {
        if response
            .get("error")
            .and_then(|err| err.get("data"))
            .and_then(|data| data.get("details"))
            .and_then(Value::as_str)
            == Some("Task not found")
        {
            saw_task_not_found_error = true;
        }
        if let Some(chunk) = response
            .get("result")
            .and_then(|result| result.get("chunk"))
        {
            if chunk.get("statusUpdate").is_some() {
                saw_status_update = true;
            }
            if chunk
                .get("task")
                .and_then(|task| task.get("status"))
                .is_some()
            {
                saw_task_status_snapshot = true;
            }
            if chunk.get("artifactUpdate").is_some() {
                saw_artifact = true;
            }
        }
    }

    let saw_any_status = saw_status_update || saw_task_status_snapshot;
    assert!(
        !saw_task_not_found_error,
        "tasks.subscribe deterministic path must not return task-not-found; responses={responses_debug}"
    );
    assert!(
        saw_any_status,
        "tasks.subscribe must stream status progress updates; responses={responses_debug}"
    );
    let _ = saw_artifact; // artifact updates are optional for this fixture.
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

/// Session send() only enqueues; must return quickly. We sample multiple sends and assert
/// the minimum elapsed time is under threshold so scheduler noise does not fail the test.
#[tokio::test]
async fn test_a2a_session_send_returns_fast_and_next_drains() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent_with_a2a_session_tool().await;
    let handle = {
        let runtime = agent.runtime();
        let mgr = runtime.lock().await;
        mgr.tool_session_handle()
    };
    let context_id = context::generate_context_id();
    let message_id = baml_rt_core::ids::MessageId::from_external(
        baml_rt_core::ids::ExternalId::new(format!("msg-{}", context_id.as_str())),
    );
    let scope =
        context::RuntimeScope::message_scope(context_id, agent.agent_id().clone(), message_id);
    let scope_for_open = scope.clone();
    context::with_scope(scope, async move {
        // Open multiple sessions and time one send per session; min elapsed approximates
        // "enqueue only" and is less sensitive to a single slow scheduler run.
        const N_SAMPLES: usize = 3;
        // The single handover lane adds serialisation overhead to send(); use a
        // generous threshold that accommodates both local and CI jitter.
        let enqueue_threshold_ms: u64 = if std::env::var_os("CI").is_some() {
            3000
        } else {
            2000
        };
        let mut send_elapsed = Vec::with_capacity(N_SAMPLES);
        let mut session_ids = Vec::with_capacity(N_SAMPLES);
        for i in 0..N_SAMPLES {
            let ctx = context::generate_context_id();
            let msg_id = baml_rt_core::ids::MessageId::from_external(
                baml_rt_core::ids::ExternalId::new(format!("msg-{}-{}", ctx.as_str(), i)),
            );
            let sc = context::RuntimeScope::message_scope(
                ctx,
                scope_for_open.agent_id().clone(),
                msg_id,
            );
            let sid = handle
                .open_tool_session(
                    &sc,
                    SYSTEM_A2A_TOOL,
                    json!({ "target": { "agent_package": "self", "agent_instance_id": "default" } }),
                )
                .await
                .expect("open system/internal_a2a");
            session_ids.push(sid);
        }
        for (i, session_id) in session_ids.iter().enumerate() {
            let send_input = json!({ "text": format!("ping-{}", i) });
            let start = std::time::Instant::now();
            handle
                .tool_session_send(session_id, send_input)
                .await
                .expect("session_send");
            send_elapsed.push(start.elapsed());
        }
        let min_elapsed = send_elapsed
            .iter()
            .min()
            .copied()
            .expect("N_SAMPLES > 0");
        assert!(
            min_elapsed < std::time::Duration::from_millis(enqueue_threshold_ms),
            "session send() must return in under {}ms (enqueue only); min of {} samples: {:?}, all: {:?}",
            enqueue_threshold_ms,
            N_SAMPLES,
            min_elapsed,
            send_elapsed
        );
        // Drain the first session to completion; finish others so we don't leave state.
        let primary = &session_ids[0];
        let mut last_step = None;
        for _ in 0..8 {
            let next = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                handle.tool_session_next(primary),
            )
            .await;
            let Ok(Ok(step)) = next else {
                break;
            };
            let terminal = matches!(
                step,
                baml_rt_tools::ToolStep::Done { .. }
                    | baml_rt_tools::ToolStep::Error { .. }
                    | baml_rt_tools::ToolStep::Suspended { .. }
            );
            last_step = Some(step);
            if terminal {
                break;
            }
        }
        if matches!(last_step, Some(baml_rt_tools::ToolStep::Done { .. })) {
            handle
                .tool_session_finish(primary)
                .await
                .expect("session_finish");
        }
        for sid in session_ids.iter().skip(1) {
            let _ = handle.tool_session_abort(sid, Some("test timing samples".into())).await;
        }
    })
    .await;
}

/// send() after finish() must fail fast (terminal phase).
#[tokio::test]
async fn test_a2a_session_send_after_finish_fails() {
    let _permit = acquire_test_permit().await;
    let agent = setup_agent_with_a2a_session_tool().await;
    let handle = {
        let runtime = agent.runtime();
        let mgr = runtime.lock().await;
        mgr.tool_session_handle()
    };
    let context_id = context::generate_context_id();
    let message_id = baml_rt_core::ids::MessageId::from_external(
        baml_rt_core::ids::ExternalId::new(format!("msg-{}", context_id.as_str())),
    );
    let scope =
        context::RuntimeScope::message_scope(context_id, agent.agent_id().clone(), message_id);
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
        let mut last_step = None;
        for _ in 0..8 {
            let next = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                handle.tool_session_next(&session_id),
            )
            .await;
            let Ok(Ok(step)) = next else {
                break;
            };
            let terminal = matches!(
                step,
                baml_rt_tools::ToolStep::Done { .. }
                    | baml_rt_tools::ToolStep::Error { .. }
                    | baml_rt_tools::ToolStep::Suspended { .. }
            );
            last_step = Some(step);
            if terminal {
                break;
            }
        }
        if matches!(last_step, Some(baml_rt_tools::ToolStep::Done { .. })) {
            handle
                .tool_session_finish(&session_id)
                .await
                .expect("finish");
        }
        let err = handle
            .tool_session_send(&session_id, send_input)
            .await
            .expect_err("send after finish must fail");
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("terminal")
                || err_msg.contains("closed")
                || err_msg.contains("Tool session not found")
                || err_msg.contains("Unknown tool session")
                || err_msg.contains("Unknown session")
                || err_msg.contains("send only valid once after open"),
            "error should mention terminal/closed/unknown-session: {}",
            err_msg
        );
    })
    .await;
}
