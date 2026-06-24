// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! A2A streaming tests that run **through QuickJS** with real fixture agents.
//!
//! These tests build a fixture package (baml-agent-builder), load the compiled agent
//! (dist/index.js) and schema, then call `handle_a2a_stream` (message.sendStream). They assert
//! on the **client-visible** stream: FSM (SUBMITTED first, then WORKING / INPUT_REQUIRED /
//! COMPLETED), task lifecycle, and that no null "stream end" is sent when suspended for input.
//!
//! Do not replace with synthetic inline JS; these exist to validate the full A2A path to QuickJS.
//!
//! ## Interface boundaries (design)
//!
//! - **Stream source**: The client receives chunks from the same broadcast channel (`output_tx`)
//!   that the transport forwards router chunks to and that `LiveStreamWorkingRelay` pushes
//!   WORKING chunks to. Session lookup uses a single key type derived only from `ContextId`
//!   at both registration and push; no fallback.
//! - **Chunk shape**: Status chunks may be flat (e.g. `make_submitted_chunk`) or nested
//!   (legacy `statusUpdate` / `status_update` wrapper). Helpers `status_update_event(su)` and
//!   `task_state_from_chunk(chunk)` handle both.
//! - **Tool WORKING contract**: At least one chunk must have `state === "TASK_STATE_WORKING"`,
//!   message text indicating the tool (e.g. "Invoking tool: support/calculate"), and when
//!   present `metadata.kind === "tool"`, `metadata.toolName === "<tool_name>"` (see
//!   `working_status_metadata_tool` in `auto_status`).

#![recursion_limit = "256"]

use std::{
    fs,
    sync::{Arc, OnceLock},
};

use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{A2aStreamChunk, A2aWireRequest, collect_a2a_stream_one_shot};
use serde_json::Value;
use test_support::common::{
    CalculatorTool, agent_fixture, build_fixture_package_to_temp, chunk_content,
    chunks_from_responses, ensure_fixture_runtime_types, message_texts_from_chunks,
    message_visible_content_from_chunks, send_stream_request,
};

fn task_state_from_chunk(chunk: &Value) -> Option<String> {
    let state_from = |v: &Value| {
        v.get("status")
            .and_then(|s| s.get("state"))
            .and_then(|s| s.as_str())
            .map(String::from)
    };
    chunk
        .get("task")
        .and_then(|task| {
            state_from(task).or_else(|| {
                task.as_str().and_then(|raw| {
                    serde_json::from_str::<Value>(raw)
                        .ok()
                        .and_then(|parsed| state_from(&parsed))
                })
            })
        })
        .or_else(|| {
            let su = chunk.get("statusUpdate")?;
            // Flat (e.g. make_submitted_chunk) has status directly; nested (StreamChunk) has statusUpdate/status_update.
            let ev = if su.get("status").is_some() {
                su
            } else {
                status_update_event(su)?
            };
            state_from(ev)
        })
}

/// Returns the status-update event body from either the flat wire shape or legacy nested aliases.
fn status_update_event(su: &Value) -> Option<&Value> {
    if su.get("status").is_some() {
        Some(su)
    } else {
        su.get("statusUpdate").or_else(|| su.get("status_update"))
    }
}

/// Inline JS that invokes support/calculate via openToolSession so ToolStarted fires and
/// we get WORKING chunks with tool metadata. Schema from stream-baml-tool; no LLM stub.
fn tool_invoker_js() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";
        if (text.startsWith("tool-working-test:")) {
            const session = await openToolSession("support/calculate");
            await session.send({ expression: { left: 2, operation: "Add", right: 3 } });
            await session.continue();
            __chat_yield({ __session: message.__session, message: { parts: [{ text: "sum=5" }] } });
            __chat_yield({ __session: message.__session, final: true });
            return;
        }
        __chat_yield({ __session: message.__session, message: { parts: [{ text: "unknown" }] } });
        __chat_yield({ __session: message.__session, final: true });
    };
    "#
    .to_string()
}

async fn collect_stream(agent: &A2aAgent, request: Value) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await?;
    let chunks: Vec<A2aStreamChunk> = collect_a2a_stream_one_shot(stream).await;
    Ok(chunks.into_iter().map(A2aStreamChunk::into_inner).collect())
}

/// Build stream-js-tool fixture package and create A2aAgent (QuickJS runs compiled TS).
async fn setup_stream_js_tool_agent() -> A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_package_to_temp("stream-js-tool").await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("stream-js-tool dist/index.js");
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build stream-js-tool agent")
}

/// Build task-lifecycle-demo fixture package and create A2aAgent.
async fn setup_task_lifecycle_demo_agent() -> A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_package_to_temp("task-lifecycle-demo").await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("task-lifecycle-demo dist/index.js");
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build task-lifecycle-demo agent")
}

/// Build emit-plan-then-block fixture package and create A2aAgent.
/// Fixture emits plan chunks, blocks event loop ~100ms (no yield), then returns.
/// Tests that plan chunks reach the client (regression for relay delay).
async fn setup_emit_plan_then_block_agent() -> A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_package_to_temp("emit-plan-then-block").await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("emit-plan-then-block dist/index.js");
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build emit-plan-then-block agent")
}

/// Build A2aAgent with stream-baml-tool schema and inline JS that invokes support/calculate
/// via openToolSession so ToolStarted fires. Used by tests that assert WORKING + tool metadata.
async fn setup_stream_baml_tool_agent() -> A2aAgent {
    ensure_fixture_runtime_types();
    let agent_dir = agent_fixture("stream-baml-tool");
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(agent_dir.to_str().expect("utf8 path"))
        .expect("load schema");
    manager
        .register_tool(CalculatorTool)
        .await
        .expect("register CalculatorTool");
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(tool_invoker_js())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build stream-baml-tool agent")
}

static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

async fn test_gate() -> tokio::sync::OwnedSemaphorePermit {
    let gate = GATE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone();
    gate.acquire_owned().await.expect("test gate")
}

/// Full stack: message.sendStream → router → QuickJS (stream-js-tool). Asserts client FSM:
/// first chunk is TASK_STATE_SUBMITTED, then we see WORKING/COMPLETED and exactly one final.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_fsm_starts_with_submitted_quickjs() {
    let _permit = test_gate().await;
    let agent = setup_stream_js_tool_agent().await;

    let request = send_stream_request(
        "msg-1",
        "stream-task: run",
        "corr-1-1",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );
    let responses = collect_stream(&agent, request).await.expect("stream");
    assert!(
        !responses.is_empty(),
        "stream must return at least one response (got 0); check fixture build and agent execution"
    );
    if let Some(err) = responses.iter().find(|r| r.get("error").is_some()) {
        panic!(
            "stream returned error response: {}",
            serde_json::to_string_pretty(err).unwrap_or_else(|_| "?".into())
        );
    }

    let chunks = chunks_from_responses(&responses);
    assert!(
        !chunks.is_empty(),
        "stream must have at least one chunk (responses: {} items)",
        responses.len()
    );

    let states: Vec<String> = chunks
        .iter()
        .filter_map(|c| task_state_from_chunk(c))
        .collect();
    assert!(
        states.first().map(|s| s.as_str()) == Some("TASK_STATE_SUBMITTED"),
        "first chunk must be SUBMITTED (client FSM); states: {:?}",
        states
    );

    assert!(
        states.contains(&"TASK_STATE_COMPLETED".to_string()),
        "stream must reach COMPLETED; states: {:?}",
        states
    );
    assert!(
        states.contains(&"TASK_STATE_WORKING".to_string()),
        "stream must include WORKING before completion; states: {:?}",
        states
    );

    let final_count = responses
        .iter()
        .filter(|r| {
            r.get("result")
                .and_then(|res| res.get("final"))
                .and_then(|f| f.as_bool())
                .unwrap_or(false)
        })
        .count();
    assert_eq!(final_count, 1, "exactly one response must have final: true");

    // stream-js-tool emits an artifact (JSON body) before the user-visible Complete line;
    // scan merged visible content, not only the first segment.
    let merged_visible = message_visible_content_from_chunks(&chunks).join("\n");
    assert!(
        merged_visible.contains("Complete:"),
        "expected completion message in merged stream text, got: {}",
        merged_visible
    );
}

/// Full stack: message.sendStream → QuickJS (task-lifecycle-demo). First turn sends "lifecycle-demo"
/// and we collect until INPUT_REQUIRED or final. Asserts: SUBMITTED first, then INPUT_REQUIRED
/// (no null stream-end chunk; stream suspends for input).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_input_required_suspends_quickjs() {
    let _permit = test_gate().await;
    let agent = setup_task_lifecycle_demo_agent().await;

    let request = send_stream_request(
        "msg-1",
        "lifecycle-demo",
        "corr-1-2",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await
        .expect("open stream");

    let stop_at_input_required = |r: &A2aStreamChunk| {
        let v = r.as_ref();
        let chunk = chunk_content(v);
        let state = chunk.and_then(task_state_from_chunk);
        let is_final = v
            .get("result")
            .and_then(|res| res.get("final"))
            .and_then(|f| f.as_bool())
            .unwrap_or(false);
        is_final || state.as_deref() == Some("TASK_STATE_INPUT_REQUIRED")
    };
    let responses: Vec<A2aStreamChunk> =
        baml_rt_core::collect_a2a_stream_until_one_shot(stream, stop_at_input_required).await;
    let responses_values: Vec<Value> = responses
        .into_iter()
        .map(A2aStreamChunk::into_inner)
        .collect();

    let chunks = chunks_from_responses(&responses_values);
    let states: Vec<String> = chunks
        .iter()
        .filter_map(|c| task_state_from_chunk(c))
        .collect();

    assert!(
        states.first().map(|s| s.as_str()) == Some("TASK_STATE_SUBMITTED"),
        "first chunk must be SUBMITTED; states: {:?}",
        states
    );

    assert!(
        states.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "first turn must emit INPUT_REQUIRED (awaitInput); states: {:?}",
        states
    );

    // When we stopped at INPUT_REQUIRED, we must not have received a final chunk (stream suspended).
    let final_count = responses_values
        .iter()
        .filter(|r| {
            r.get("result")
                .and_then(|res| res.get("final"))
                .and_then(|f| f.as_bool())
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        final_count, 0,
        "INPUT_REQUIRED turn must not have final: true (stream suspended, not ended)"
    );
}

/// Matrix test: resume-to-terminal (COMPLETED and FAILED) for task-lifecycle-demo.
///
/// One agent, two scenarios with distinct context_ids:
/// - **Completed**: turn1 "lifecycle-demo" → INPUT_REQUIRED; turn2 "review-path" → … → turn4 "confirm" → COMPLETED.
/// - **Failed**: turn1 "lifecycle-demo" → INPUT_REQUIRED; turn2 "fail-now" → FAILED (no COMPLETED).
///
/// If the fail-now scenario sees 0 states on turn2, the second request is not resuming the same
/// session/task (e.g. scope or task_id not propagated); both scenarios use the same live path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_input_required_resume_to_terminal_quickjs() {
    let _permit = test_gate().await;
    let agent = setup_task_lifecycle_demo_agent().await;

    // ---- Scenario: resume to COMPLETED (4 turns) ----
    let context_id_completed = baml_rt_core::ids::ContextId::new(9, 1);
    let turn1 = send_stream_request(
        "msg-9-1",
        "lifecycle-demo",
        "corr-9-1",
        Some(context_id_completed.clone()),
    );
    let r1 = collect_stream(&agent, turn1).await.expect("turn1 stream");
    let s1: Vec<String> = chunks_from_responses(&r1)
        .into_iter()
        .filter_map(task_state_from_chunk)
        .collect();
    assert_eq!(
        s1.first().map(|s| s.as_str()),
        Some("TASK_STATE_SUBMITTED"),
        "turn1 must begin with SUBMITTED; states: {:?}",
        s1
    );
    assert!(
        s1.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "turn1 must suspend for input; states: {:?}",
        s1
    );
    assert_eq!(
        r1.iter()
            .filter(|r| r
                .get("result")
                .and_then(|x| x.get("final"))
                .and_then(Value::as_bool)
                == Some(true))
            .count(),
        0,
        "turn1 must not emit final: true (suspended)"
    );

    let turn2 = send_stream_request(
        "msg-9-2",
        "review-path",
        "corr-9-2",
        Some(context_id_completed.clone()),
    );
    let r2 = collect_stream(&agent, turn2).await.expect("turn2 stream");
    let s2: Vec<String> = chunks_from_responses(&r2)
        .into_iter()
        .filter_map(task_state_from_chunk)
        .collect();
    assert!(
        s2.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "turn2 must suspend for review decision; states: {:?}",
        s2
    );

    let turn3 = send_stream_request(
        "msg-9-3",
        "approve",
        "corr-9-3",
        Some(context_id_completed.clone()),
    );
    let _r3 = collect_stream(&agent, turn3).await.expect("turn3 stream");

    let turn4 = send_stream_request(
        "msg-9-4",
        "confirm",
        "corr-9-4",
        Some(context_id_completed.clone()),
    );
    let r4 = collect_stream(&agent, turn4).await.expect("turn4 stream");
    let s4: Vec<String> = chunks_from_responses(&r4)
        .into_iter()
        .filter_map(task_state_from_chunk)
        .collect();
    assert!(
        s4.contains(&"TASK_STATE_COMPLETED".to_string()),
        "completed scenario: turn4 must reach COMPLETED; states: {:?}",
        s4
    );
    assert_eq!(
        r4.iter()
            .filter(|r| {
                r.get("result")
                    .and_then(|x| x.get("final"))
                    .and_then(Value::as_bool)
                    == Some(true)
            })
            .count(),
        1,
        "terminal turn must emit exactly one final: true"
    );

    // ---- Scenario: resume to FAILED (2 turns) ----
    let context_id_failed = baml_rt_core::ids::ContextId::new(10, 1);
    let turn1_f = send_stream_request(
        "msg-10-1",
        "lifecycle-demo",
        "corr-10-1",
        Some(context_id_failed.clone()),
    );
    let r1_f = collect_stream(&agent, turn1_f)
        .await
        .expect("turn1 failed scenario");
    let s1_f: Vec<String> = chunks_from_responses(&r1_f)
        .into_iter()
        .filter_map(task_state_from_chunk)
        .collect();
    assert!(
        s1_f.contains(&"TASK_STATE_INPUT_REQUIRED".to_string()),
        "failed scenario turn1 must suspend; states: {:?}",
        s1_f
    );

    let turn2_f = send_stream_request(
        "msg-10-2",
        "fail-now",
        "corr-10-2",
        Some(context_id_failed.clone()),
    );
    let r2_f = collect_stream(&agent, turn2_f)
        .await
        .expect("turn2 failed scenario");
    let s2_f: Vec<String> = chunks_from_responses(&r2_f)
        .into_iter()
        .filter_map(task_state_from_chunk)
        .collect();
    assert!(
        s2_f.contains(&"TASK_STATE_FAILED".to_string()),
        "failed scenario turn2 must reach FAILED; states: {:?}",
        s2_f
    );
    assert!(
        !s2_f.contains(&"TASK_STATE_COMPLETED".to_string()),
        "failed scenario must not emit COMPLETED; states: {:?}",
        s2_f
    );
    assert_eq!(
        r2_f.iter()
            .filter(|r| r
                .get("result")
                .and_then(|x| x.get("final"))
                .and_then(Value::as_bool)
                == Some(true))
            .count(),
        1,
        "failed terminal turn must emit exactly one final: true"
    );
}

/// **Tool-call WORKING status on the live stream**
///
/// This test verifies that when the agent invokes a tool via the **session path**
/// (`openToolSession` → `session.send` → `session.continue`), the HTTP A2A live stream
/// delivers at least one `TASK_STATE_WORKING` status chunk that:
///
/// 1. **Message text**: Contains `"Invoking tool: support/calculate"` so clients can show
///    human-readable progress without parsing metadata.
/// 2. **Metadata**: When present, includes `metadata.kind === "tool"` and
///    `metadata.toolName === "support/calculate"` so clients can treat the chunk as a
///    tool call (e.g. for UI or analytics) without parsing the message string.
///
/// **Stack under test**
///
/// - Request: `message.sendStream` with a user message whose text starts with
///   `"tool-working-test:"`.
/// - Agent: stream-baml-tool schema, inline JS (`tool_invoker_js()`), no LLM; JS calls
///   `openToolSession("support/calculate")`, `session.send(...)`, `session.continue()`,
///   then yields a message and final.
/// - Effect path: QuickJS `tool_session_send` → `EffectEmitter::start_tool` → effect bus →
///   `LiveStreamWorkingRelay::on_effect` → formatted chunk sent to the session’s
///   `output_tx` (same channel the client stream consumes).
///
/// **What is *not* tested here**
///
/// - Single-shot `execute_tool` path (ToolStarted is also emitted there; this test
///   focuses on the session path).
/// - LLM WORKING chunks or their metadata.
/// - Ordering of WORKING vs SUBMITTED vs message chunks (only presence of a valid
///   WORKING chunk with tool metadata is required).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_tool_working_quickjs() {
    let _permit = test_gate().await;
    let agent = setup_stream_baml_tool_agent().await;

    let request = send_stream_request(
        "msg-1",
        "tool-working-test: run",
        "corr-2-1",
        Some(baml_rt_core::ids::ContextId::new(2, 1)),
    );
    let responses = collect_stream(&agent, request).await.expect("stream");
    assert!(
        !responses.is_empty(),
        "stream must return at least one response"
    );
    if let Some(err) = responses.iter().find(|r| r.get("error").is_some()) {
        panic!(
            "stream returned error response: {}",
            serde_json::to_string_pretty(err).unwrap_or_else(|_| "?".into())
        );
    }

    let chunks = chunks_from_responses(&responses);
    // Find any WORKING chunk whose message text indicates a tool call.
    let working_chunks: Vec<_> = chunks
        .iter()
        .filter_map(|c| {
            let su = c.get("statusUpdate")?;
            let ev = status_update_event(su)?;
            let state = ev.get("status")?.get("state")?.as_str()?;
            if state != "TASK_STATE_WORKING" {
                return None;
            }
            let text = ev
                .get("status")
                .and_then(|s| s.get("message"))
                .and_then(|m| m.get("parts"))
                .and_then(|p| p.get(0))
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if text.contains("Invoking tool:") && text.contains("support/calculate") {
                Some((ev, text))
            } else {
                None
            }
        })
        .collect();

    assert!(
        !working_chunks.is_empty(),
        "stream must contain at least one WORKING statusUpdate with message 'Invoking tool: support/calculate'; chunks (states): {:?}",
        chunks
            .iter()
            .filter_map(|c| {
                c.get("statusUpdate")
                    .and_then(|su| status_update_event(su))
                    .and_then(|ev| ev.get("status")?.get("state")?.as_str())
            })
            .collect::<Vec<_>>()
    );

    // Metadata is part of the relay contract for tool WORKING chunks.
    for (ev, text) in &working_chunks {
        let meta = ev
            .get("metadata")
            .expect("tool WORKING chunk must include metadata");
        assert_eq!(
            meta.get("kind").and_then(Value::as_str),
            Some("tool"),
            "WORKING chunk metadata.kind must be 'tool'"
        );
        assert_eq!(
            meta.get("toolName").and_then(Value::as_str),
            Some("support/calculate"),
            "WORKING chunk metadata.toolName must be 'support/calculate'"
        );
        assert!(
            text.contains("Invoking tool:") && text.contains("support/calculate"),
            "WORKING chunk message must indicate tool invocation; got: {}",
            text
        );
    }
}

/// **Relay pushes WORKING when client omits contextId (session key vs scope must match)**
///
/// When the client does not send `contextId` on the first message, the server generates one
/// for the live stream session. Requests in that session are then handled in `run_live_stream_session`;
/// the scope used for routing must use the *session's* context_id (the one that keys the session),
/// not a freshly parsed/generated one from the request body. Otherwise the relay would call
/// `push_working_to_session(scope_context_id)` and find no session.
///
/// The fix: pass the session's context_id into `handle_a2a_outcome_inner` as
/// `session_context_id_override` so the scope is built from it. This test sends the first request
/// with **no** contextId and asserts that WORKING chunks still arrive; it would fail if the
/// override were not passed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_tool_working_quickjs_no_context_id() {
    let _permit = test_gate().await;
    let agent = setup_stream_baml_tool_agent().await;

    // Omit contextId so session gets a server-generated id; scope must use that id (session_context_id_override).
    let request = send_stream_request("msg-no-ctx", "tool-working-test: run", "corr-1-1", None);
    let responses = collect_stream(&agent, request).await.expect("stream");
    assert!(
        !responses.is_empty(),
        "stream must return at least one response"
    );
    if let Some(err) = responses.iter().find(|r| r.get("error").is_some()) {
        panic!(
            "stream returned error response: {}",
            serde_json::to_string_pretty(err).unwrap_or_else(|_| "?".into())
        );
    }

    let chunks = chunks_from_responses(&responses);
    let working_chunks: Vec<_> = chunks
        .iter()
        .filter_map(|c| {
            let su = c.get("statusUpdate")?;
            let ev = status_update_event(su)?;
            let state = ev.get("status")?.get("state")?.as_str()?;
            if state != "TASK_STATE_WORKING" {
                return None;
            }
            let text = ev
                .get("status")
                .and_then(|s| s.get("message"))
                .and_then(|m| m.get("parts"))
                .and_then(|p| p.get(0))
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if text.contains("Invoking tool:") && text.contains("support/calculate") {
                Some(())
            } else {
                None
            }
        })
        .collect();

    assert!(
        !working_chunks.is_empty(),
        "with no contextId in first request, relay must still push WORKING (session key must match scope); \
         got {} WORKING chunks; chunk states: {:?}",
        working_chunks.len(),
        chunks
            .iter()
            .filter_map(|c| {
                c.get("statusUpdate")
                    .and_then(|su| status_update_event(su))
                    .and_then(|ev| ev.get("status")?.get("state")?.as_str())
            })
            .collect::<Vec<_>>()
    );
}

/// **Effects and message chunks arrive in real time and in causal order**
///
/// This test encodes the requirement that A2A consumers receive stream chunks as they
/// are produced: effects (e.g. tool WORKING status) and message chunks must appear in
/// **causal order** and **during** execution, not buffered or reordered.
///
/// This test uses the same agent as `test_a2a_stream_tool_working_quickjs`: it invokes
/// `openToolSession("support/calculate")`, then `session.send` / `session.continue()`,
/// then yields a message "sum=5" and final. Causal order is:
///
///   1. SUBMITTED (task accepted)
///   2. WORKING (ToolStarted → relay pushes status)
///   3. Message chunk "sum=5" (yielded after tool completes)
///   4. COMPLETED / final
///
/// We assert:
/// - Each of these chunk types appears at least once.
/// - Their **indices** in the stream are strictly increasing: submitted < working <
///   message_sum < completed. That guarantees the client sees effects before the
///   message content that was produced after the effect.
/// - There are at least two chunks before any final chunk, so delivery is not
///   "all at end" (real-time requirement).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_effects_and_messages_real_time_ordered_quickjs() {
    let _permit = test_gate().await;
    let agent = setup_stream_baml_tool_agent().await;

    let request = send_stream_request(
        "msg-1",
        "tool-working-test: run",
        "corr-3000-1",
        Some(baml_rt_core::ids::ContextId::new(2, 2)),
    );
    let responses = collect_stream(&agent, request).await.expect("stream");
    assert!(
        !responses.is_empty(),
        "stream must return at least one response"
    );
    if let Some(err) = responses.iter().find(|r| r.get("error").is_some()) {
        panic!(
            "stream returned error response: {}",
            serde_json::to_string_pretty(err).unwrap_or_else(|_| "?".into())
        );
    }

    let chunks = chunks_from_responses(&responses);
    assert!(
        chunks.len() >= 2,
        "stream must deliver at least two chunks (real-time requirement); got {}",
        chunks.len()
    );

    let idx_submitted = chunks
        .iter()
        .position(|c| task_state_from_chunk(c).as_deref() == Some("TASK_STATE_SUBMITTED"));
    let idx_working = chunks.iter().position(|c| {
        let su = match c.get("statusUpdate") {
            Some(s) => s,
            None => return false,
        };
        let ev = match status_update_event(su) {
            Some(e) => e,
            None => return false,
        };
        let state = match ev
            .get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
        {
            Some(s) => s,
            None => return false,
        };
        if state != "TASK_STATE_WORKING" {
            return false;
        }
        let text = ev
            .get("status")
            .and_then(|s| s.get("message"))
            .and_then(|m| m.get("parts"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        text.contains("Invoking tool:") && text.contains("support/calculate")
    });
    let idx_message_sum = chunks.iter().position(|c| {
        c.get("message")
            .and_then(|m| m.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|p| p.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            .map(|t| t.contains("sum=5"))
            .unwrap_or(false)
    });
    let idx_completed = chunks
        .iter()
        .position(|c| task_state_from_chunk(c).as_deref() == Some("TASK_STATE_COMPLETED"));

    assert!(
        idx_submitted.is_some(),
        "stream must contain SUBMITTED; chunk states: {:?}",
        chunks
            .iter()
            .filter_map(|c| task_state_from_chunk(c))
            .collect::<Vec<_>>()
    );
    assert!(
        idx_working.is_some(),
        "stream must contain WORKING (tool) status; chunk states: {:?}",
        chunks
            .iter()
            .filter_map(|c| task_state_from_chunk(c))
            .collect::<Vec<_>>()
    );
    assert!(
        idx_message_sum.is_some(),
        "stream must contain message chunk 'sum=5'; message_texts: {:?}",
        message_texts_from_chunks(&chunks)
    );
    assert!(
        idx_completed.is_some(),
        "stream must reach COMPLETED; chunk states: {:?}",
        chunks
            .iter()
            .filter_map(|c| task_state_from_chunk(c))
            .collect::<Vec<_>>()
    );

    let s = idx_submitted.unwrap();
    let w = idx_working.unwrap();
    let m = idx_message_sum.unwrap();
    let c = idx_completed.unwrap();

    assert!(
        s < w,
        "SUBMITTED must appear before WORKING (causal order); submitted_idx={} working_idx={}",
        s,
        w
    );
    assert!(
        w < m,
        "WORKING (tool) must appear before message 'sum=5' (effect before content produced after tool); working_idx={} message_idx={}",
        w,
        m
    );
    assert!(
        m < c,
        "message 'sum=5' must appear before COMPLETED; message_idx={} completed_idx={}",
        m,
        c
    );

    // Real-time: at least one non-final chunk exists before the final one (delivery during execution).
    let idx_final = chunks
        .iter()
        .position(|c| c.get("final").and_then(Value::as_bool).unwrap_or(false));
    if let Some(f) = idx_final {
        assert!(
            f >= 2,
            "final chunk must not be among the first two (chunks must arrive during execution); final_idx={}",
            f
        );
    }
}

/// **Plan chunks reach client when emitted before blocking work**
///
/// Regression test for relay delay: the emit-plan-then-block fixture emits plan chunks
/// (message, statusChanged) with NO yield, then blocks the event loop ~100ms, then returns.
/// Without a concurrent drain or emit yield, the collector cannot drain until advance returns.
///
/// Asserts that the plan chunks ("--- Plan ---", "Starting development.") are delivered
/// to the client stream. Documents the intended behavior; a future concurrent-drain fix
/// should keep this passing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a2a_stream_emit_plan_then_block_chunks_reach_client_quickjs() {
    let _permit = test_gate().await;
    let agent = setup_emit_plan_then_block_agent().await;

    let request = send_stream_request(
        "msg-1",
        "plan-then-block",
        "corr-100-1",
        Some(baml_rt_core::ids::ContextId::new(11, 1)),
    );
    let responses = collect_stream(&agent, request).await.expect("stream");
    assert!(
        !responses.is_empty(),
        "stream must return at least one response"
    );
    if let Some(err) = responses.iter().find(|r| r.get("error").is_some()) {
        panic!(
            "stream returned error response: {}",
            serde_json::to_string_pretty(err).unwrap_or_else(|_| "?".into())
        );
    }

    let chunks = chunks_from_responses(&responses);
    let message_texts = message_texts_from_chunks(&chunks);

    assert!(
        message_texts.iter().any(|t| t.contains("--- Plan ---")),
        "stream must contain plan message (--- Plan ---); message_texts: {:?}",
        message_texts
    );
    assert!(
        message_texts
            .iter()
            .any(|t| t.contains("Starting development.")),
        "stream must contain 'Starting development.'; message_texts: {:?}",
        message_texts
    );

    let states: Vec<String> = chunks
        .iter()
        .filter_map(|c| task_state_from_chunk(c))
        .collect();
    assert!(
        states.contains(&"TASK_STATE_COMPLETED".to_string()),
        "stream must reach COMPLETED; states: {:?}",
        states
    );
}
