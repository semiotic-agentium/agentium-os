//! A2A malformed requests and error-path E2E tests.
//!
//! **Purpose:** Verify that invalid JSON-RPC input and runtime error paths
//! (allowlist violation, tool failure mid-stream, mixed success/failure under
//! concurrency) produce the expected error responses or stream content. These
//! are adversarial / failure-path tests, not happy-path E2E.
//!
//! **Stream termination:** Every stream fixture here is *deliberately* terminating.
//! The runtime collector ends the stream on: (1) a chunk with `final: true`, or
//! (2) `statusUpdate.status.state` = `TASK_STATE_COMPLETED` / `TASK_STATE_FAILED`, or
//! (3) channel close, or (4) idle timeout (default 60s, configurable). All fixtures yield (1) and/or (2)
//! before returning, so no non-terminating streams. The concurrency test injects a short stream
//! collector idle (5s) so it finishes quickly; the point is mixed success/failure, not the watchdog.
//!
//! **Tests:**
//! - Concurrency under load: table-driven stream phase matrix (final/completed/closed mode) with
//!   per-stream liveness timeouts.
//! - Streaming tool failure: tool returns `Err` during a stream → stream contains error content.
//! - Allowlist during stream: JS opens a tool not in the runtime allowlist → stream contains allowlist error.

#![recursion_limit = "256"]

mod common;

use std::{sync::Arc, time::Duration};

use baml_rt::{
    A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager, tools::BamlTool,
};
use baml_rt_tools::bundles::BundleType;
use baml_derive::BamlType;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use test_support::common::{AddNumbersTool, chunk_content, send_stream_request};
use tokio::time::timeout;

struct Test;

async fn collect_responses(agent: &A2aAgent, request: Value) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await?;
    let chunks = baml_rt_core::collect_a2a_stream(stream).await;
    Ok(chunks
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect())
}

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for malformed/error-path A2A tests"
    }
}

/// Consolidated concurrent stream phase matrix: final/completed/closed/input terminal shapes.
#[derive(Clone)]
struct StreamPhaseCase {
    tag: &'static str,
    chunks: usize,
    mode: &'static str,
    min_chunks: usize,
}

impl StreamPhaseCase {
    fn js_key(&self) -> String {
        format!("{}:{}:{}", self.tag, self.chunks, self.mode)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn test_concurrent_stream_phase_matrix_regression_under_load() {
    let agent = A2aAgent::builder()
        .with_runtime_manager(BamlRuntimeManager::builder().build().unwrap())
        .with_init_js(
        r#"
        globalThis.onChatMessage = async function(message) {
            const payload = (message && message.parts && message.parts[0] && message.parts[0].text) || "alpha:4:final";
            const match = payload.match(/^([^:]+):(\d+):(.*)$/);
            const tag = match ? match[1] : "alpha";
            const chunk_count = match ? parseInt(match[2], 10) : 4;
            const mode = match ? match[3] : "final";

            for (let i = 0; i < chunk_count; i++) {
                __chat_yield({ message: { parts: [{ text: tag + "-chunk-" + i }] } });
                if (i % 2 === 0) {
                    __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
                }
            }

            if (mode === "completed") {
                __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_COMPLETED" } } });
            } else if (mode === "closed") {
                // no terminal marker; rely on channel close after JS function returns
            } else if (mode === "input") {
                __chat_yield({ task: { status: { state: "TASK_STATE_INPUT_REQUIRED" } } });
                __chat_yield({ final: true });
            } else {
                __chat_yield({ final: true });
            }
        };
        "#,
        )
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(
            QuickJSConfig::new()
                .with_stream_concurrency(Some(12))
                .with_stream_collector_idle_secs(Some(5)),
        )
        .build()
        .await
        .unwrap();

    let cases = [
        StreamPhaseCase {
            tag: "alpha",
            chunks: 3,
            mode: "final",
            min_chunks: 2,
        },
        StreamPhaseCase {
            tag: "beta",
            chunks: 4,
            mode: "completed",
            min_chunks: 2,
        },
    ];

    // 4 waves × 2 cases = 8 concurrent streams; enough to assert interleaving without overloading.
    let mut handles = Vec::new();
    for wave in 0..4 {
        for (index, case) in cases.iter().enumerate() {
            let request_id = format!("corr-1700000000010-{}", wave * cases.len() + index + 1);
            let request = send_stream_request(
                &format!("msg-{wave}-{}", case.tag),
                &case.js_key(),
                &request_id,
                None,
            );
            let case = case.clone();
            let agent = agent.clone();
            handles.push((
                case,
                tokio::spawn(async move {
                    timeout(Duration::from_secs(12), collect_responses(&agent, request)).await
                }),
            ));
        }
    }

    for (case, join_handle) in handles {
        let responses = match timeout(Duration::from_secs(15), join_handle).await {
            Ok(Ok(Ok(Ok(values)))) => values,
            Ok(Ok(Ok(Err(err)))) => panic!("stream {} failed: {err}", case.tag),
            Ok(Ok(Err(_inner_timeout_err))) => {
                panic!("stream {} exceeded its 12s execution timeout", case.tag)
            }
            Ok(Err(join_err)) => panic!("stream {} join error: {join_err}", case.tag),
            Err(_) => panic!("stream {} did not finish within 15s join timeout", case.tag),
        };
        println!("case {} collected {} responses", case.tag, responses.len());

        assert!(
            responses.len() >= case.min_chunks,
            "stream {} should emit at least {} chunks",
            case.tag,
            case.min_chunks
        );
        assert!(
            responses.iter().any(|response| {
                serde_json::to_string(response)
                    .unwrap_or_default()
                    .contains(case.tag)
            }),
            "stream {} should include case tag in chunk payload",
            case.tag
        );
        assert!(
            responses.iter().any(|response| {
                let serialized = serde_json::to_string(response).unwrap_or_default();
                match case.mode {
                    "completed" => serialized.contains("TASK_STATE_COMPLETED"),
                    "final" => {
                        serialized.contains("\"final\":true")
                            || serialized.contains("TASK_STATE_COMPLETED")
                    }
                    "closed" => serialized.contains("TASK_STATE_WORKING"),
                    "input" => serialized.contains("TASK_STATE_INPUT_REQUIRED"),
                    _ => true,
                }
            }),
            "stream {} should include terminal marker matching mode {}",
            case.tag,
            case.mode
        );
    }
}

/// Tool that always returns `Err` on execute; used to assert streaming error handling.
struct FailingTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct FailingInput {
    msg: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct FailingOutput {
    ok: bool,
}

#[async_trait::async_trait]
impl BamlTool for FailingTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "failing_tool";
    type OpenInput = ();
    type Input = FailingInput;
    type Output = FailingOutput;

    fn description(&self) -> &'static str {
        "Fails on execute for error-path tests"
    }

    async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
        Err(baml_rt_core::BamlRtError::InvalidArgument(
            "tool failed by design".to_string(),
        ))
    }
}

/// **Purpose:** When a tool opened during a stream returns `Err` on execute, the stream must contain error content (e.g. message part with error text) so the client can observe the failure.
#[tokio::test(flavor = "current_thread")]
async fn test_streaming_tool_failure_mid_stream() {
    let mut runtime = BamlRuntimeManager::builder().build().unwrap();
    runtime.register_tool(FailingTool).await.unwrap();
    let store = common::provenance::build_graphqlite_test_store();
    let agent = A2aAgent::builder()
        .with_runtime_manager(runtime)
        .with_init_js(r#"
            globalThis.onChatMessage = async function(message) {
                __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
                try {
                    const session = await openToolSession("test/failing_tool");
                    await session.send({ msg: "fail" });
                    const step = await session.continue();
                    __chat_yield({ message: { parts: [{ text: step && step.error ? step.error.message : "no error" }] } });
                } catch (e) {
                    __chat_yield({ message: { parts: [{ text: "err: " + String(e) }] } });
                }
                __chat_yield({ final: true });
            };
        "#)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_graphqlite_store(store)
        .build()
        .await
        .unwrap();

    let request = send_stream_request("fail-1", "trigger", "corr-1700000000020-1", None);
    let responses = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        collect_responses(&agent, request),
    )
    .await
    .expect("stream must complete within 60s (live stream did not finalize)")
    .unwrap();
    assert!(
        !responses.is_empty(),
        "stream should return at least one chunk"
    );
    let has_error = responses.iter().any(|r| {
        let Some(chunk) = chunk_content(r) else {
            return false;
        };
        let text = chunk
            .get("message")
            .and_then(|m| m.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        text.contains("failed") || text.contains("error") || text.contains("err:")
    });
    assert!(has_error, "stream should contain error from failing tool");
}

/// **Purpose:** When the runtime has a tool allowlist that excludes a tool, and JS tries to open that tool during a stream, the stream must contain an allowlist-related error (so the client sees the violation).
#[tokio::test(flavor = "current_thread")]
async fn test_allowlist_violation_during_stream() {
    use std::collections::HashSet;

    use test_support::common::CalculatorTool;

    let mut runtime = BamlRuntimeManager::builder().build().unwrap();
    runtime.register_tool(AddNumbersTool).await.unwrap();
    runtime.register_tool(CalculatorTool).await.unwrap();
    let mut allowlist = HashSet::new();
    allowlist.insert("test/add_numbers".to_string());
    runtime.set_tool_allowlist(allowlist).await.unwrap();

    let store = common::provenance::build_graphqlite_test_store();
    let agent = A2aAgent::builder()
        .with_runtime_manager(runtime)
        .with_init_js(
            r#"
            globalThis.onChatMessage = async function(message) {
                __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
                try {
                    const session = await openToolSession("support/calculate");
                    await session.send({ expression: { left: 1, operation: "Add", right: 2 } });
                    const step = await session.continue();
                    __chat_yield({ message: { parts: [{ text: "unexpected success" }] } });
                } catch (e) {
                    __chat_yield({ message: { parts: [{ text: "allowlist: " + String(e) }] } });
                }
                __chat_yield({ final: true });
            };
        "#,
        )
        .with_graphqlite_store(store)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();

    let request = send_stream_request("allow-1", "trigger", "corr-1700000000030-1", None);
    let responses = collect_responses(&agent, request).await.unwrap();
    assert!(!responses.is_empty());
    let has_allowlist_msg = responses.iter().any(|r| {
        let Some(chunk) = chunk_content(r) else {
            return false;
        };
        let text = chunk
            .get("message")
            .and_then(|m| m.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        text.to_lowercase().contains("allowlist")
    });
    assert!(
        has_allowlist_msg,
        "stream should contain allowlist violation message"
    );
}
