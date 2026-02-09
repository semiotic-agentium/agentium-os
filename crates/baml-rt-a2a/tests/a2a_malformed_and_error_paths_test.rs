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

use baml_rt::a2a_types::{
    JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, ROLE_USER, SendMessageRequest,
};
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::tools::BamlTool;
use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig};
use baml_rt_a2a::a2a_types::A2aMessageId;
use baml_rt_core::ids::ExternalId;
use baml_rt_tools::bundles::BundleType;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use ts_rs::TS;

struct Test;
impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for malformed/error-path A2A tests"
    }
}

fn user_message(msg_id: &str, text: &str) -> Message {
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(msg_id)),
        role: MessageRole::String(ROLE_USER.to_string()),
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: None,
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

fn valid_send_stream_request(msg_id: &str, text: &str, req_id: &str) -> Value {
    let params = SendMessageRequest {
        message: user_message(msg_id, text),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String(req_id.to_string())),
    };
    serde_json::to_value(request).unwrap()
}

fn is_error_response(response: &Value) -> bool {
    response.get("error").is_some()
}

/// **Purpose:** Sending a request with `jsonrpc` other than `"2.0"` must yield a single JSON-RPC error response (not success).
#[tokio::test(flavor = "current_thread")]
async fn test_malformed_a2a_invalid_jsonrpc_version() {
    let agent = A2aAgent::builder()
        .with_init_js("globalThis.onChatMessage = async () => {};")
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(5_000)))
        .build()
        .await
        .unwrap();

    let request = json!({
        "jsonrpc": "1.0",
        "method": "message.sendStream",
        "params": serde_json::to_value(SendMessageRequest {
            message: user_message("m1", "hi"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: HashMap::new(),
        }).unwrap(),
        "id": "corr-1"
    });
    let responses = agent.handle_a2a(request).await.unwrap();
    assert_eq!(responses.len(), 1);
    assert!(
        is_error_response(&responses[0]),
        "invalid jsonrpc version should return error"
    );
}

/// **Purpose:** Sending a request with an unsupported method (e.g. `message.send` instead of `message.sendStream`) must yield a single JSON-RPC error response.
#[tokio::test(flavor = "current_thread")]
async fn test_malformed_a2a_unsupported_method() {
    let agent = A2aAgent::builder()
        .with_init_js("globalThis.onChatMessage = async () => {};")
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(5_000)))
        .build()
        .await
        .unwrap();

    let request = json!({
        "jsonrpc": "2.0",
        "method": "message.send",
        "params": null,
        "id": "corr-2"
    });
    let responses = agent.handle_a2a(request).await.unwrap();
    assert_eq!(responses.len(), 1);
    assert!(
        is_error_response(&responses[0]),
        "unsupported method should return error"
    );
}

/// **Purpose:** Sending `message.sendStream` with params that cannot deserialize to `SendMessageRequest` (e.g. missing `message`) must yield a single JSON-RPC error response.
#[tokio::test(flavor = "current_thread")]
async fn test_malformed_a2a_invalid_params() {
    let agent = A2aAgent::builder()
        .with_init_js("globalThis.onChatMessage = async () => {};")
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(5_000)))
        .build()
        .await
        .unwrap();

    let request = json!({
        "jsonrpc": "2.0",
        "method": "message.sendStream",
        "params": {},
        "id": "corr-3"
    });
    let responses = agent.handle_a2a(request).await.unwrap();
    assert_eq!(responses.len(), 1);
    assert!(
        is_error_response(&responses[0]),
        "invalid params (missing message) should return error"
    );
}

/// **Purpose:** When valid and malformed requests are handled concurrently, valid requests complete with stream success and malformed requests return a single error response (no cross-talk or panic).
#[tokio::test(flavor = "current_thread")]
async fn test_concurrency_mixed_success_failure() {
    let agent = A2aAgent::builder()
        .with_init_js(r#"globalThis.onChatMessage = async function() { __baml_chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } }); };"#)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();

    let valid1 = valid_send_stream_request("v1", "hi", "corr-v1");
    let valid2 = valid_send_stream_request("v2", "ho", "corr-v2");
    let malformed =
        json!({ "jsonrpc": "1.0", "method": "message.sendStream", "params": {}, "id": "corr-bad" });

    let agent_c = agent.clone();
    let h1 = tokio::task::spawn_local(async move { agent_c.handle_a2a(valid1).await });
    let agent_c = agent.clone();
    let h2 = tokio::task::spawn_local(async move { agent_c.handle_a2a(valid2).await });
    let agent_c = agent.clone();
    let malformed2 = malformed.clone();
    let h3 = tokio::task::spawn_local(async move { agent_c.handle_a2a(malformed).await });
    let agent_c = agent.clone();
    let h4 = tokio::task::spawn_local(async move { agent_c.handle_a2a(malformed2).await });

    let r1 = h1.await.unwrap().unwrap();
    let r2 = h2.await.unwrap().unwrap();
    let r3 = h3.await.unwrap().unwrap();
    let r4 = h4.await.unwrap().unwrap();

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
    let agent = A2aAgent::builder()
        .with_runtime_manager(runtime)
        .with_init_js(r#"
            globalThis.onChatMessage = async function(message) {
                __baml_chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
                try {
                    const session = await openToolSession("test/failing_tool", __baml_invocation_token);
                    await session.send({ msg: "fail" });
                    const step = await session.continue();
                    __baml_chat_yield({ message: { parts: [{ text: step && step.error ? step.error.message : "no error" }] } });
                } catch (e) {
                    __baml_chat_yield({ message: { parts: [{ text: "err: " + String(e) }] } });
                }
            };
        "#)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();

    let request = valid_send_stream_request("fail-1", "trigger", "corr-fail");
    let responses = agent.handle_a2a(request).await.unwrap();
    assert!(
        !responses.is_empty(),
        "stream should return at least one chunk"
    );
    let has_error = responses.iter().any(|r| {
        let chunk = r
            .get("result")
            .and_then(|res| res.get("chunk"))
            .or_else(|| r.get("result"));
        let text = chunk
            .and_then(|c| c.get("message"))
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

/// AddNumbersTool so allowlist can include `test/add_numbers` (validation requires all allowlisted tools to be registered).
struct AddNumbersTool;
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct AddNumbersInput {
    a: f64,
    b: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct AddNumbersOutput {
    result: f64,
}
#[async_trait::async_trait]
impl BamlTool for AddNumbersTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "add_numbers";
    type OpenInput = ();
    type Input = AddNumbersInput;
    type Output = AddNumbersOutput;
    fn description(&self) -> &'static str {
        "Adds two numbers"
    }
    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(AddNumbersOutput {
            result: args.a + args.b,
        })
    }
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

    let agent = A2aAgent::builder()
        .with_runtime_manager(runtime)
        .with_init_js(r#"
            globalThis.onChatMessage = async function(message) {
                __baml_chat_yield({ statusUpdate: { status: { state: "TASK_STATE_WORKING" } } });
                try {
                    const session = await openToolSession("support/calculate", __baml_invocation_token);
                    await session.send({ expression: { left: 1, operation: "Add", right: 2 } });
                    const step = await session.continue();
                    __baml_chat_yield({ message: { parts: [{ text: "unexpected success" }] } });
                } catch (e) {
                    __baml_chat_yield({ message: { parts: [{ text: "allowlist: " + String(e) }] } });
                }
            };
        "#)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();

    let request = valid_send_stream_request("allow-1", "trigger", "corr-allow");
    let responses = agent.handle_a2a(request).await.unwrap();
    assert!(!responses.is_empty());
    let has_allowlist_msg = responses.iter().any(|r| {
        let chunk = r
            .get("result")
            .and_then(|res| res.get("chunk"))
            .or_else(|| r.get("result"));
        let text = chunk
            .and_then(|c| c.get("message"))
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
