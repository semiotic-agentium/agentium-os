//! Contract tests for BAML function invocation results
//!
//! These tests assert on the actual structure and content of results,
//! ensuring the contract between JavaScript/BAML functions and the runtime is correct.
//! Uses fixture `stream-baml-tool` and BAML function `ChooseCalcTool` (returns session plan object).
//!
//! **Authority:** One test per boundary — direct BAML invoke, and JS wrapper that calls BAML.
//! Do not add further contract tests with the same setup/assertions (see testing-handbook authority map).

use baml_rt_core::context::{self, InvocationScope};
use serde_json::json;

use test_support::common::{
    assert_result_contract_actual_result, ensure_fixture_runtime_types,
    setup_stream_baml_tool_agent_for_contract,
};

#[tokio::test]
async fn test_baml_function_returns_actual_result() {
    // Contract: invoke_function must return the actual BAML result (not wrapped in success object).
    ensure_fixture_runtime_types();
    let agent = setup_stream_baml_tool_agent_for_contract(None).await;
    let bridge_handle = agent.bridge();
    let scope = InvocationScope::synthetic_message(agent.agent_id().clone());

    let result = context::with_scope(scope.as_scope().clone(), async {
        let mut bridge = bridge_handle.lock().await;
        bridge
            .invoke_function(
                &scope,
                "ChooseCalcTool",
                json!({"user_message": "compute 2+3"}),
            )
            .await
    })
    .await;

    match result {
        Ok(val) => assert_result_contract_actual_result(&val),
        Err(e) => panic!(
            "Unexpected error: {}. Contract violation: function should return actual result.",
            e
        ),
    }
}

#[tokio::test]
async fn test_js_function_invocation_returns_actual_result() {
    // Contract: When invoking a JS function that calls BAML, the result must be the actual BAML result, not a success wrapper.
    ensure_fixture_runtime_types();
    let get_calc_plan_js = r#"
        async function getCalcPlan(args) {
            return await ChooseCalcTool({
                user_message: args.message || "compute 2+3",
                __baml_invocation_token: args.__baml_invocation_token
            });
        }
        globalThis.getCalcPlan = getCalcPlan;
    "#;
    let agent = setup_stream_baml_tool_agent_for_contract(Some(get_calc_plan_js)).await;
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;
    let scope = InvocationScope::synthetic_message(agent.agent_id().clone());

    let result = bridge
        .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
        .await;

    match result {
        Ok(val) => assert_result_contract_actual_result(&val),
        Err(e) => panic!(
            "CONTRACT VIOLATION: Unexpected error: {}. Function should return actual result.",
            e
        ),
    }
}
