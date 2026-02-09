//! Contract tests for BAML function invocation results
//!
//! These tests assert on the actual structure and content of results,
//! ensuring the contract between JavaScript/BAML functions and the runtime is correct.
//! Uses fixture `stream-baml-tool` and BAML function `ChooseCalcTool` (returns session plan object).
//!
//! Use short max_attempts_ms so effect-gated poll doesn't hang when LLM is used (e.g. missing API key).

use baml_rt::A2aAgent;
use baml_rt::QuickJSConfig;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::context::{self, InvocationScope};
use serde_json::json;
use std::fs;
use std::sync::Arc;

use test_support::common::{CalculatorTool, agent_fixture, ensure_fixture_runtime_types};

/// QuickJS config for tests: short max_attempts so effect-gated poll doesn't hang (LLM fixtures).
fn test_quickjs_config() -> QuickJSConfig {
    QuickJSConfig::new().with_max_attempts_ms(Some(15_000)) // 15s instead of 30 min
}

#[tokio::test]
async fn test_baml_function_returns_actual_result() {
    // Contract: invoke_function must return the actual BAML result (not wrapped in success object).
    // Uses stream-baml-tool and ChooseCalcTool which returns a session plan object with "steps".
    ensure_fixture_runtime_types();

    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    let agent_dir = agent_fixture("stream-baml-tool");
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let scope = InvocationScope::standalone(agent.agent_id().clone());
    let result = context::with_scope(scope.as_scope().clone(), async {
        let mut bridge = bridge_handle.lock().await;
        bridge
            .invoke_function("ChooseCalcTool", json!({"user_message": "compute 2+3"}))
            .await
    })
    .await;

    // Contract assertion: Result must be the actual value (plan with "steps" or tool output with "result"/"formatted"), not a wrapper
    match result {
        Ok(val) => {
            assert!(
                val.is_object(),
                "Expected object result, got: {:?}. Function must return actual result.",
                val
            );
            let obj = val.as_object().unwrap();
            assert!(
                !obj.contains_key("success"),
                "CONTRACT VIOLATION: Result must not be wrapped in success object, got: {:?}",
                val
            );
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            assert!(
                has_steps || has_tool_output,
                "Expected object with 'steps' (plan) or 'result'/'formatted' (tool output), got: {:?}",
                val
            );
        }
        Err(e) => {
            panic!(
                "Unexpected error: {}. Contract violation: function should return actual result.",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_js_function_invocation_returns_actual_result() {
    // Contract: When invoking a JS function that calls BAML, the result must be the actual BAML result, not a success wrapper.
    ensure_fixture_runtime_types();

    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    let agent_dir = agent_fixture("stream-baml-tool");
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = r#"
        async function getCalcPlan(args) {
            return await ChooseCalcTool({
                user_message: args.message || "compute 2+3",
                __baml_invocation_token: args.__baml_invocation_token
            });
        }
        globalThis.getCalcPlan = getCalcPlan;
    "#;
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;
    let scope = InvocationScope::standalone(agent.agent_id().clone());

    let result = bridge
        .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
        .await;

    match result {
        Ok(val) => {
            assert!(
                val.is_object(),
                "CONTRACT VIOLATION: Expected actual result (object), got: {:?}. Must not be wrapped.",
                val
            );
            let obj = val.as_object().unwrap();
            assert!(
                !obj.contains_key("success"),
                "CONTRACT VIOLATION: Result must not contain 'success' wrapper, got: {:?}",
                val
            );
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            assert!(
                has_steps || has_tool_output,
                "Expected object with 'steps' or 'result'/'formatted', got: {:?}",
                val
            );
        }
        Err(e) => {
            panic!(
                "CONTRACT VIOLATION: Unexpected error: {}. Function should return actual result.",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_invoke_function_api_contract() {
    // Contract: invoke_function API must return the actual function result, not wrapped in any success object.
    ensure_fixture_runtime_types();

    let agent_dir = agent_fixture("stream-baml-tool");
    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = r#"
        async function getCalcPlan(args) {
            return await ChooseCalcTool({
                user_message: args.message || "compute 2+3",
                __baml_invocation_token: args.__baml_invocation_token
            });
        }
        globalThis.getCalcPlan = getCalcPlan;
    "#;
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;
    let scope = InvocationScope::standalone(agent.agent_id().clone());

    let result = bridge
        .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
        .await;

    match result {
        Ok(val) => {
            assert!(
                val.is_object(),
                "CONTRACT VIOLATION: API must return actual result (object), not wrapper. Got: {:?}",
                val
            );
            let obj = val.as_object().unwrap();
            assert!(
                obj.get("success").is_none(),
                "CONTRACT VIOLATION: Result must not contain 'success' field: {:?}",
                val
            );
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            assert!(
                has_steps || has_tool_output,
                "Expected object with 'steps' or 'result'/'formatted', got: {:?}",
                val
            );
        }
        Err(e) => {
            let error_str = format!("{}", e);
            if error_str.contains("Promise did not resolve") {
                panic!("CONTRACT VIOLATION: Promise resolution failed: {}", e);
            }
            panic!("CONTRACT VIOLATION: Unexpected error: {}", e);
        }
    }
}

#[tokio::test]
async fn test_loaded_agent_invoke_function_contract() {
    // Contract: LoadedAgent::invoke_function must return the actual result, not wrapped (same pattern as load_agent_package).
    ensure_fixture_runtime_types();

    let agent_dir = test_support::common::agent_fixture("stream-baml-tool");

    let mut runtime_manager = BamlRuntimeManager::new().unwrap();
    runtime_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    runtime_manager.register_tool(CalculatorTool).await.unwrap();

    // Use dist/index.js only (compiled JS). Do not load src/index.ts (TypeScript) - QuickJS cannot parse it.
    let dist_path = agent_dir.join("dist").join("index.js");
    let mut agent_code = if dist_path.exists() {
        fs::read_to_string(&dist_path).unwrap()
    } else {
        String::new()
    };
    if !agent_code.contains("globalThis.getCalcPlan") {
        agent_code.push_str(
            r#"
            async function getCalcPlan(args) {
                return await ChooseCalcTool({
                    user_message: args.message || "compute 2+3",
                    __baml_invocation_token: args.__baml_invocation_token
                });
            }
            globalThis.getCalcPlan = getCalcPlan;
        "#,
        );
    }
    let agent = A2aAgent::builder()
        .with_runtime_manager(runtime_manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;
    let scope = InvocationScope::standalone(agent.agent_id().clone());

    let result = bridge
        .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
        .await;

    match result {
        Ok(val) => {
            assert!(
                val.is_object(),
                "CONTRACT VIOLATION: invoke_function must return actual result (object), got: {:?}",
                val
            );
            let obj = val.as_object().unwrap();
            assert!(
                !obj.contains_key("success"),
                "CONTRACT VIOLATION: Result must not be success wrapper: {:?}",
                val
            );
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            assert!(
                has_steps || has_tool_output,
                "Expected object with 'steps' or 'result'/'formatted', got: {:?}",
                val
            );
        }
        Err(e) => {
            panic!(
                "CONTRACT VIOLATION: Unexpected error: {}. Function should return actual result.",
                e
            );
        }
    }
}
