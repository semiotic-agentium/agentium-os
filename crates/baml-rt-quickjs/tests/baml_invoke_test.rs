//! Tests for JavaScript invocation of BAML functions.
//! LLM calls are stubbed so no API key is required.

#![recursion_limit = "256"]

use baml_rt::interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor};
use baml_rt_core::{
    context,
    ids::{AgentId, UuidId},
};
use serde_json::json;
use test_support::common::{
    CalculatorTool, agent_fixture, ensure_fixture_runtime_types, setup_baml_runtime_from_fixture,
    setup_bridge,
};

/// Stub interceptor: returns a canned session plan for ChooseCalcTool so no real LLM call is made.
struct StubChooseCalcToolInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubChooseCalcToolInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_name == "ChooseCalcTool" {
            Ok(InterceptorDecision::Substitute(json!({
                "step": {
                    "op": "Send",
                    "input": {
                        "expression": {
                            "left": 2,
                            "operation": "Add",
                            "right": 3
                        }
                    },
                    "reason": "stub send"
                }
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

#[tokio::test]
async fn test_js_invoke_baml_function() {
    ensure_fixture_runtime_types();
    let agent_dir = agent_fixture("stream-baml-tool");
    if !agent_dir.join("baml_src").exists() {
        eprintln!("Skipping test: stream-baml-tool fixture not found");
        return;
    }

    let baml_manager = setup_baml_runtime_from_fixture("stream-baml-tool");
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(CalculatorTool).await.unwrap();
        manager
            .register_llm_interceptor(StubChooseCalcToolInterceptor)
            .await;
    }
    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Test invoking ChooseCalcTool from JavaScript (stream-baml-tool has this function)
    // Use __awaitAndStringify helper to handle async function calls
    let js_code = r#"
        (function() {
            try {
                const promise = ChooseCalcTool({ user_message: "add 2 and 3" });
                return __awaitAndStringify(promise);
            } catch (e) {
                return JSON.stringify({ success: false, error: e.toString() });
            }
        })()
    "#;

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000b1").unwrap());
    let scope = context::InvocationScope::synthetic_message(agent_id);

    let result = context::with_scope(scope.as_scope().clone(), async {
        bridge.eval_scoped(&scope, js_code).await
    })
    .await;

    let json_result = result.expect("JS evaluate should succeed");
    println!("JavaScript execution result: {:?}", json_result);

    // Strict mode: stub returns one Send fragment; runtime executes it and returns sent status.
    let obj = json_result
        .as_object()
        .expect("Result should be a JSON object");
    assert!(
        obj.get("status").and_then(|v| v.as_str()) == Some("sent"),
        "Result should contain strict sent status; got keys: {:?}",
        obj.keys().collect::<Vec<_>>()
    );
}
