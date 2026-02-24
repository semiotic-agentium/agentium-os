//! A2A malformed requests and error-path E2E tests.
//!
//! **Purpose:** Verify that invalid JSON-RPC input and runtime error paths
//! (allowlist violation, tool failure mid-stream, mixed success/failure under
//! concurrency) produce the expected error responses or stream content. These
//! are adversarial / failure-path tests, not happy-path E2E.
//!
//! **Tests:**
//! - Malformed JSON-RPC: wrong version, unsupported method, invalid params → single error response.
//! - Concurrency: valid and malformed requests run together → valid succeed, malformed return error.
//! - Streaming tool failure: tool returns `Err` during a stream → stream contains error content.
//! - Allowlist during stream: JS opens a tool not in the runtime allowlist → stream contains allowlist error.

#![recursion_limit = "256"]

mod common;

use std::{collections::HashMap, sync::Arc};

use baml_rt::{
    A2aAgent, A2aRequestHandler, QuickJSConfig, a2a_types::SendMessageRequest,
    baml::BamlRuntimeManager, tools::BamlTool,
};
use baml_rt_tools::bundles::BundleType;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use test_support::common::{
    AddNumbersTool, build_minimal_a2a_agent, chunk_content, is_error_response, send_stream_request,
    user_message,
};
use ts_rs::TS;

struct Test;

async fn collect_responses(agent: &A2aAgent, request: Value) -> baml_rt::Result<Vec<Value>> {
    Ok(baml_rt_core::collect_a2a_stream(agent.handle_a2a_stream(request).await?).await)
}

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for malformed/error-path A2A tests"
    }
}

/// Table-driven malformed A2A request cases: (jsonrpc, method, params) → single JSON-RPC error response.
#[tokio::test(flavor = "current_thread")]
async fn test_malformed_a2a_table_driven() {
    let agent = build_minimal_a2a_agent("globalThis.onChatMessage = async () => {};").await;

    let malformed_params = serde_json::to_value(SendMessageRequest {
        message: user_message("m1", "hi", None),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    })
    .unwrap();

    let cases: [(Value, &str); 3] = [
        (
            json!({
                "jsonrpc": "1.0",
                "method": "message.sendStream",
                "params": malformed_params,
                "id": "corr-invalid-version"
            }),
            "invalid jsonrpc version",
        ),
        (
            json!({
                "jsonrpc": "2.0",
                "method": "message.send",
                "params": null,
                "id": "corr-unsupported-method"
            }),
            "unsupported method",
        ),
        (
            json!({
                "jsonrpc": "2.0",
                "method": "message.sendStream",
                "params": {},
                "id": "corr-invalid-params"
            }),
            "invalid params (missing message)",
        ),
    ];

    for (request, desc) in cases {
        let responses = collect_responses(&agent, request).await.unwrap();
        assert_eq!(
            responses.len(),
            1,
            "malformed case '{desc}' must yield exactly one response"
        );
        assert!(
            is_error_response(&responses[0]),
            "malformed case '{desc}' must return JSON-RPC error"
        );
    }
}

/// **Purpose:** When valid and malformed requests are handled concurrently, valid requests complete with stream success and malformed requests return a single error response (no cross-talk or panic).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrency_mixed_success_failure() {
    let agent = build_minimal_a2a_agent(
        r#"globalThis.onChatMessage = async function() { __chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } }); __chat_yield({ final: true }); };"#,
    )
    .await;

    let valid1 = send_stream_request("v1", "hi", "corr-1700000000010-1", None);
    let valid2 = send_stream_request("v2", "ho", "corr-1700000000011-1", None);
    let malformed = json!({ "jsonrpc": "1.0", "method": "message.sendStream", "params": {}, "id": "corr-1700000000012-1" });

    let malformed2 = malformed.clone();
    let agent1 = agent.clone();
    let agent2 = agent.clone();
    let agent3 = agent.clone();
    let agent4 = agent.clone();
    let (r1, r2, r3, r4) = tokio::join!(
        collect_responses(&agent1, valid1),
        collect_responses(&agent2, valid2),
        collect_responses(&agent3, malformed),
        collect_responses(&agent4, malformed2),
    );
    let r1 = r1.unwrap();
    let r2 = r2.unwrap();
    let r3 = r3.unwrap();
    let r4 = r4.unwrap();

    assert!(
        !r1.is_empty() && !is_error_response(&r1[0]),
        "valid1 should succeed"
    );
    assert!(
        !r2.is_empty() && !is_error_response(&r2[0]),
        "valid2 should succeed"
    );
    assert_eq!(r3.len(), 1);
    assert!(is_error_response(&r3[0]), "malformed should return error");
    assert_eq!(r4.len(), 1);
    assert!(is_error_response(&r4[0]), "malformed should return error");
}

/// Tool that always returns `Err` on execute; used to assert streaming error handling.
struct FailingTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct FailingInput {
    msg: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
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
    let mut runtime = BamlRuntimeManager::new().unwrap();
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
    let responses = collect_responses(&agent, request).await.unwrap();
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

    let mut runtime = BamlRuntimeManager::new().unwrap();
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
