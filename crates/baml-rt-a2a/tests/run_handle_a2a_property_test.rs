//! Property tests for `A2aRequestHandler::handle_a2a` behavior.
//!
//! This suite deliberately uses malformed A2A requests so the handler returns JSON-RPC
//! error responses without relying on JS integration behavior.
//!
//! Invariant:
//!   ∀ submitted request r_i, exactly one response envelope is returned.
//! Liveness:
//!   ∀ submitted request r_i, completion occurs within bounded time.

#![recursion_limit = "256"]

mod common;

use std::sync::Arc;

use baml_rt::interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor};
use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::ContextId;
use proptest::prelude::*;
use serde_json::json;
use test_support::common::{
    CalculatorTool, agent_fixture, ensure_fixture_runtime_types, first_message_text_from_stream,
    is_error_response, send_stream_request,
};
use tokio::{
    task::JoinSet,
    time::{Duration, sleep, timeout},
};

fn proptest_cfg(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(cases);
    // Integration tests do not have a crate root (lib.rs/main.rs) for source-based persistence.
    // Disable persistence to avoid noisy "failed to find lib.rs or main.rs" warnings.
    cfg.failure_persistence = None;
    cfg
}

async fn collect_responses(
    agent: &A2aAgent,
    request: serde_json::Value,
) -> baml_rt::Result<Vec<serde_json::Value>> {
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await?;
    let chunks = baml_rt_core::collect_a2a_stream(stream).await;
    Ok(chunks
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect())
}

fn response_has_input_required(response: &serde_json::Value) -> bool {
    response
        .get("result")
        .and_then(|r| r.get("chunk"))
        .and_then(|c| c.get("task"))
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("state"))
        .and_then(serde_json::Value::as_str)
        .map(|s| s == "TASK_STATE_INPUT_REQUIRED")
        .unwrap_or(false)
}

struct StubChooseCalcToolInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubChooseCalcToolInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_name == "ChooseCalcTool" {
            Ok(InterceptorDecision::Substitute(json!({
                "steps": [
                    { "op": "Open", "reason": "stub open" },
                    {
                        "op": "Send",
                        "input": {
                            "expression": {
                                "left": 2,
                                "operation": "Add",
                                "right": 3
                            }
                        },
                        "reason": "stub send"
                    },
                    { "op": "Next", "reason": "stub next" },
                    { "op": "Finish", "reason": "stub finish" }
                ]
            })))
        } else {
            Ok(InterceptorDecision::Allow)
        }
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &baml_rt_core::Result<serde_json::Value>,
        _duration_ms: u64,
    ) {
    }
}

fn interleaving_js_handler() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const tag = (chunk) => (message?.__session !== undefined ? Object.assign({}, chunk, { __session: message.__session }) : chunk);
        const text = message?.parts?.[0]?.text || "";
        const [kind, hopsRaw, contextToken] = text.split(":");
        const contextId = contextToken || "no-context";
        const hops = Number.isFinite(Number(hopsRaw)) ? Math.min(Math.max(parseInt(hopsRaw, 10), 0), 7) : 0;
        for (let i = 0; i < hops; i++) {
            await Promise.resolve();
        }
        if (kind === "a2a") {
            __chat_yield(tag({ message: { parts: [{ text: `A2A:${contextId}` }] } }));
            __chat_yield(tag({ final: true }));
            return;
        }
        if (kind === "input-ask") {
            __chat_yield(tag({
                task: {
                    status: { state: "TASK_STATE_INPUT_REQUIRED" },
                    metadata: { prompt: "Need additional input" }
                }
            }));
            return;
        }
        if (kind === "input-answer") {
            __chat_yield(tag({ message: { parts: [{ text: `INPUT:${contextId}` }] } }));
            __chat_yield(tag({ final: true }));
            return;
        }
        if (kind === "tool") {
            const session = await openToolSession("support/calculate");
            await session.send({ expression: { left: 2, operation: "Add", right: 3 } });
            const step = await session.continue();
            const result = step?.output?.result ?? 5;
            __chat_yield(tag({ message: { parts: [{ text: `TOOL:${contextId}:${result}` }] } }));
            __chat_yield(tag({ final: true }));
            return;
        }
        // kind === "llm" (or fallback)
        const plan = await ChooseCalcTool({ user_message: "compute 2+3" });
        const stepCount = Array.isArray(plan?.steps) ? plan.steps.length : 0;
        __chat_yield(tag({ message: { parts: [{ text: `LLM:${contextId}:${stepCount}` }] } }));
        __chat_yield(tag({ final: true }));
    };
    "#
    .to_string()
}

async fn setup_interleaving_agent() -> A2aAgent {
    ensure_fixture_runtime_types();
    let mut manager = baml_rt::BamlRuntimeManager::new().expect("create manager");
    let agent_dir = agent_fixture("stream-baml-tool");
    manager
        .load_schema(agent_dir.to_str().expect("fixture path"))
        .expect("load fixture schema");
    manager
        .register_tool(CalculatorTool)
        .await
        .expect("register calculator tool");
    manager
        .register_llm_interceptor(StubChooseCalcToolInterceptor)
        .await;
    let store = common::provenance::build_graphqlite_test_store();
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(interleaving_js_handler())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(
            baml_rt::QuickJSConfig::new()
                .with_idle_timeout_ms(Some(45_000))
                .with_max_attempts_ms(Some(45_000)),
        )
        .with_graphqlite_store(store)
        .build()
        .await
        .expect("build interleaving agent")
}

/// Single-context run of the INPUT_REQUIRED resume flow (no proptest).
/// Verifies fixture and expectations; use to debug prop_input_required_resume_positive_and_no_auto_final.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_input_required_resume_single_context() {
    let agent = setup_interleaving_agent().await;
    let context_id = ContextId::new(901, 1);
    let jitter_ms = 0u64;

    // Turn 1: ask for input, stop collection once INPUT_REQUIRED appears.
    let ask_req = send_stream_request(
        "msg-ir-ask-0",
        &format!("input-ask:{jitter_ms}:{}", context_id.as_str()),
        "corr-1700000000300-1",
        Some(context_id.clone()),
    );
    let ask_stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(ask_req))
        .await
        .expect("open input-required stream");
    let ask_responses: Vec<baml_rt_core::A2aStreamChunk> = timeout(
        Duration::from_secs(10),
        baml_rt_core::collect_a2a_stream_until(ask_stream, |c| {
            response_has_input_required(c.as_ref())
        }),
    )
    .await
    .expect("input-required stream timed out");
    assert!(
        ask_responses
            .iter()
            .any(|c| response_has_input_required(c.as_ref())),
        "first turn must emit TASK_STATE_INPUT_REQUIRED"
    );
    let ask_final_count = ask_responses
        .iter()
        .filter(|chunk| {
            chunk
                .as_ref()
                .get("result")
                .and_then(|result| result.get("final"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        ask_final_count, 0,
        "input-required turn must not auto-finalize"
    );

    tokio::time::sleep(Duration::from_millis(jitter_ms)).await;

    // Turn 2: resume same context and expect terminal completion.
    let answer_req = send_stream_request(
        "msg-ir-answer-0",
        &format!("input-answer:{jitter_ms}:{}", context_id.as_str()),
        "corr-1700000000400-1",
        Some(context_id.clone()),
    );
    let answer_responses = timeout(
        Duration::from_secs(10),
        collect_responses(&agent, answer_req),
    )
    .await
    .expect("resumed stream timed out")
    .expect("resumed stream failed");
    let answer_final_count = answer_responses
        .iter()
        .filter(|value| {
            value
                .get("result")
                .and_then(|result| result.get("final"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        answer_final_count, 1,
        "resumed turn must include exactly one final chunk"
    );
    let text = first_message_text_from_stream(&answer_responses);
    assert!(
        text.starts_with("INPUT:"),
        "resumed turn should emit INPUT:* message, got: {text}"
    );
    assert!(
        text.contains(context_id.as_str()),
        "resumed turn must keep context attribution: expected {}, got {text}",
        context_id.as_str()
    );
}

proptest! {
    #![proptest_config(proptest_cfg(6))]

    /// PROPERTY:
    /// ∀ N malformed requests submitted through handle_a2a:
    ///   - each future resolves within T
    ///   - each result contains exactly one JSON-RPC error response
    #[test]
    fn prop_run_handle_a2a_malformed_requests_are_bounded_and_single_response(n in 1u32..=12u32) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        rt.block_on(async move {
            let store = common::provenance::build_graphqlite_test_store();
            let agent = A2aAgent::builder()
                .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
                .with_graphqlite_store(store)
                .build()
                .await
                .expect("agent build");

            for i in 0..n {
                let malformed_request = json!({ "foo": format!("bad-{i}") });
                let timed = timeout(Duration::from_secs(2), collect_responses(&agent, malformed_request))
                    .await
                    .expect("handle_a2a timeout");
                let responses = timed.expect("handle_a2a result");
                assert_eq!(responses.len(), 1, "exactly one response envelope");
                let response = &responses[0];
                assert!(
                    is_error_response(response),
                    "malformed request must produce JSON-RPC error envelope: {response}"
                );
            }
        });
    }

    /// PROPERTY (consolidated interleavings):
    /// ∀ concurrent requests over distinct contexts with kinds ∈ {a2a, tool, llm} and small jitter:
    ///   - each request resolves within bounded time
    ///   - each stream has exactly one final marker
    ///   - response text is scoped to its own context (no cross-contamination)
    ///
    /// Timeout scales with ops.len() because stream handling is serialized per bridge (one permit):
    /// the last request may not start until all earlier ones complete. Ops capped at 10 to keep
    /// test duration bounded (see run_handle_a2a_property_test_ANALYSIS.md).
    #[test]
    fn prop_interleaved_a2a_tool_llm_multi_context_isolation(
        ops in prop::collection::vec((0u8..=2u8, 0u8..=7u8), 3..=10)
    ) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        // Per-request timeout must allow for serialization: last request waits for (ops.len()-1)
        // others. Use (ops.len() * 3) + 5s so even the last request has time to run.
        let timeout_secs = (ops.len() * 3) + 5;
        let request_timeout = Duration::from_secs(timeout_secs as u64);

        rt.block_on(async move {
            let agent = setup_interleaving_agent().await;
            let mut join_set = JoinSet::new();
            for (idx, (kind_raw, jitter_raw)) in ops.iter().copied().enumerate() {
                let agent = agent.clone();
                let context_id = ContextId::new(900, (idx as u64) + 1);
                let kind = match kind_raw {
                    0 => "a2a",
                    1 => "tool",
                    _ => "llm",
                };
                let jitter_ms = (jitter_raw as u64) + ((idx as u64) % 3);
                let request = send_stream_request(
                    &format!("msg-{idx}"),
                    &format!("{kind}:{jitter_ms}:{}", context_id.as_str()),
                    &format!("corr-1700000000200-{}", idx + 1),
                    Some(context_id.clone()),
                );
                join_set.spawn(async move {
                    sleep(Duration::from_millis(jitter_ms)).await;
                    let responses = timeout(request_timeout, collect_responses(&agent, request))
                        .await
                        .expect("interleaving request timed out (serialized streams)")
                        .expect("interleaving request failed");
                    (kind.to_string(), context_id, responses)
                });
            }

            let mut completed = 0usize;
            while let Some(result) = join_set.join_next().await {
                let (kind, context_id, responses) = result.expect("join");
                completed += 1;
                assert!(!responses.is_empty(), "responses must not be empty");
                let final_count = responses
                    .iter()
                    .filter(|value| {
                        value
                            .get("result")
                            .and_then(|result| result.get("final"))
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                    })
                    .count();
                assert_eq!(final_count, 1, "stream must include exactly one final marker");

                let text = first_message_text_from_stream(&responses);
                let expected_prefix = match kind.as_str() {
                    "a2a" => "A2A:",
                    "tool" => "TOOL:",
                    _ => "LLM:",
                };
                assert!(
                    text.starts_with(expected_prefix),
                    "message text prefix mismatch for kind {kind}: {text}"
                );
                assert!(
                    text.contains(context_id.as_str()),
                    "context contamination: expected {ctx} in {text}",
                    ctx = context_id.as_str(),
                );
            }
            assert_eq!(completed, ops.len(), "all spawned requests must complete");
        });
    }

    /// PROPERTY (INPUT_REQUIRED):
    /// ∀ contexts c:
    ///   - first turn `input-ask` yields INPUT_REQUIRED and does NOT auto-finalize
    ///   - second turn `input-answer` in same context resumes and reaches exactly one final chunk
    ///
    /// Single-context scenario is also validated by `test_input_required_resume_single_context`.
    #[test]
    fn prop_input_required_resume_positive_and_no_auto_final(
        jitters in prop::collection::vec(0u8..=7u8, 1..=8)
    ) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        rt.block_on(async move {
            let agent = setup_interleaving_agent().await;
            let mut join_set = JoinSet::new();
            for (idx, jitter_raw) in jitters.iter().copied().enumerate() {
                let agent = agent.clone();
                let context_id = ContextId::new(901, (idx as u64) + 1);
                let jitter_ms = (jitter_raw as u64) + ((idx as u64) % 3);
                join_set.spawn(async move {
                    // Turn 1: ask for input, stop collection once INPUT_REQUIRED appears.
                    let ask_req = send_stream_request(
                        &format!("msg-ir-ask-{idx}"),
                        &format!("input-ask:{jitter_ms}:{}", context_id.as_str()),
                        &format!("corr-1700000000300-{}", idx + 1),
                        Some(context_id.clone()),
                    );
                    let ask_stream = agent
                        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(ask_req))
                        .await
                        .expect("open input-required stream");
                    let ask_responses: Vec<baml_rt_core::A2aStreamChunk> = timeout(
                        Duration::from_secs(6),
                        baml_rt_core::collect_a2a_stream_until(ask_stream, |c| response_has_input_required(c.as_ref())),
                    )
                    .await
                    .expect("input-required stream timed out");
                    assert!(
                        ask_responses.iter().any(|c| response_has_input_required(c.as_ref())),
                        "first turn must emit TASK_STATE_INPUT_REQUIRED"
                    );
                    let ask_final_count = ask_responses
                        .iter()
                        .filter(|chunk| {
                            chunk
                                .as_ref()
                                .get("result")
                                .and_then(|result| result.get("final"))
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false)
                        })
                        .count();
                    assert_eq!(ask_final_count, 0, "input-required turn must not auto-finalize");

                    sleep(Duration::from_millis(jitter_ms)).await;

                    // Turn 2: resume same context and expect terminal completion.
                    let answer_req = send_stream_request(
                        &format!("msg-ir-answer-{idx}"),
                        &format!("input-answer:{jitter_ms}:{}", context_id.as_str()),
                        &format!("corr-1700000000400-{}", idx + 1),
                        Some(context_id.clone()),
                    );
                    let answer_responses = timeout(Duration::from_secs(6), collect_responses(&agent, answer_req))
                        .await
                        .expect("resumed stream timed out")
                        .expect("resumed stream failed");
                    let answer_final_count = answer_responses
                        .iter()
                        .filter(|value| {
                            value
                                .get("result")
                                .and_then(|result| result.get("final"))
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false)
                        })
                        .count();
                    assert_eq!(
                        answer_final_count, 1,
                        "resumed turn must include exactly one final chunk"
                    );
                    let text = first_message_text_from_stream(&answer_responses);
                    assert!(
                        text.starts_with("INPUT:"),
                        "resumed turn should emit INPUT:* message, got: {text}"
                    );
                    assert!(
                        text.contains(context_id.as_str()),
                        "resumed turn must keep context attribution: expected {ctx}, got {text}",
                        ctx = context_id.as_str()
                    );
                });
            }

            let mut completed = 0usize;
            while let Some(res) = join_set.join_next().await {
                res.expect("join");
                completed += 1;
            }
            assert_eq!(completed, jitters.len(), "all input-required scenarios must complete");
        });
    }
}
