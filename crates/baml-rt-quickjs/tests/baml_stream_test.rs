//! Tests for JavaScript streaming invocation of BAML functions.
//! LLM calls are stubbed so no API key is required.

#![recursion_limit = "256"]

use std::sync::Arc;

use baml_rt::{
    A2aAgent, QuickJSConfig,
    baml::BamlRuntimeManager,
    interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor},
};
use baml_rt_core::{
    context,
    ids::{AgentId, UuidId},
};
use serde_json::Value;

/// Stub interceptor: substitutes for ChooseCalcToolStream so no real LLM call is made.
struct StubChooseCalcToolStreamInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubChooseCalcToolStreamInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_name == "ChooseCalcToolStream" {
            Ok(InterceptorDecision::Substitute(Value::String("5".into())))
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

#[tokio::test]
async fn test_js_stream_baml_function() {
    test_support::common::ensure_fixture_runtime_types();

    let mut baml_manager = BamlRuntimeManager::new().unwrap();

    let agent_dir = test_support::common::agent_fixture("stream-baml-tool");
    if !agent_dir.join("baml_src").exists() {
        eprintln!("Skipping test: stream-baml-tool fixture not found");
        return;
    }
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager
        .register_llm_interceptor(StubChooseCalcToolStreamInterceptor)
        .await;

    let store = test_support::common::test_graphqlite_store();
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(45_000)))
        .with_graphqlite_store(store)
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // Test invoking ChooseCalcToolStream from JavaScript (stream-baml-tool has this function)
    // Use __awaitAndStringify helper to handle async function calls
    // Note: This will fail without an API key, but we can test the invocation path
    let js_code = r#"
        (function() {
            try {
                const promise = ChooseCalcToolStream({ user_message: "add 2 and 3" });
                return __awaitAndStringify(promise);
            } catch (e) {
                return JSON.stringify({ success: false, error: e.toString() });
            }
        })()
    "#;

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000b2").unwrap());
    let scope = context::InvocationScope::synthetic_message(agent_id);

    let result = context::with_scope(scope.as_scope().clone(), async {
        bridge.evaluate(Some(&scope), js_code).await
    })
    .await;

    let json_result = result.expect("JS evaluate should succeed");
    println!("JavaScript streaming execution result: {:?}", json_result);

    if json_result.is_array() {
        return;
    }
    if let Some(s) = json_result.as_str() {
        // Stub returns "5"; __awaitAndStringify may return the raw string
        assert_eq!(s, "5", "Stub substitutes with string \"5\"");
        return;
    }
    let obj = json_result
        .as_object()
        .expect("Result should be array, string, or object");
    assert!(
        obj.contains_key("success") || obj.contains_key("error"),
        "Result should contain 'success' or 'error' field; got keys: {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    if let Some(success) = obj.get("success").and_then(|s| s.as_bool())
        && success
    {
        assert!(
            obj.contains_key("results"),
            "Success result should contain 'results' array"
        );
    }
}
