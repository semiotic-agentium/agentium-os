//! Contract tests for BAML function invocation results
//!
//! These tests assert on the actual structure and content of results,
//! ensuring the contract between JavaScript/BAML functions and the runtime is correct.
//! Uses fixture `stream-baml-tool` and BAML function `ChooseCalcTool` (returns session plan object).
//!
//! Parse failures from LLM output are retried automatically (up to 3 attempts) in baml_execution.
//!
//! Use bounded max_attempts_ms so effect-gated poll doesn't hang when LLM is used (e.g. missing API key).
//! Must be long enough for combined retries: parse retry (3 attempts) + BAML client retry can exceed 15s.

use baml_rt::A2aAgent;
use baml_rt::QuickJSConfig;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::bus::{BusWithEffects, EffectEmitter, EffectLiveness};
use baml_rt_core::context::{self, InvocationScope};
use serde_json::json;
use std::fs;
use std::sync::Arc;

use test_support::common::{
    CalculatorTool, agent_fixture, require_fixture_runtime_types, run_live_llm_with_retry_validate,
};

/// QuickJS config for LLM-dependent contract tests. Timeout must accommodate combined retries
/// (parse retry + BAML client retry) which can exceed 15s; 45s provides margin.
fn test_quickjs_config() -> QuickJSConfig {
    QuickJSConfig::new()
        .with_idle_timeout_ms(Some(45_000))
        .with_max_attempts_ms(Some(45_000))
}

fn load_env() {
    let _ = dotenvy::dotenv();
}

async fn wire_bridge_effect_liveness(agent: &A2aAgent, bus: Arc<BusWithEffects>) {
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;
    bridge.set_effect_liveness(bus as Arc<dyn EffectLiveness>);
}

#[tokio::test]
async fn test_baml_function_returns_actual_result() {
    // Contract: invoke_function must return the actual BAML result (not wrapped in success object).
    // Uses stream-baml-tool and ChooseCalcTool which returns a session plan object with "steps".
    load_env();
    require_fixture_runtime_types();

    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    let agent_dir = agent_fixture("stream-baml-tool");
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let effect_bus = Arc::new(BusWithEffects::new());
    baml_manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>)
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    wire_bridge_effect_liveness(&agent, effect_bus).await;
    let bridge_handle = agent.bridge();
    let scope = InvocationScope::synthetic_message(agent.agent_id().clone());
    let result = run_live_llm_with_retry_validate(
        "ChooseCalcTool contract invoke",
        3,
        std::time::Duration::from_secs(120),
        |_| {
            let scope = scope.clone();
            let bridge_handle = bridge_handle.clone();
            async move {
                context::with_scope(scope.as_scope().clone(), async {
                    let mut bridge = bridge_handle.lock().await;
                    bridge
                        .invoke_function(
                            &scope,
                            "ChooseCalcTool",
                            json!({"user_message": "compute 2+3"}),
                        )
                        .await
                })
                .await
            }
        },
        |val| {
            if !val.is_object() {
                return Err(format!("Expected object result, got: {val:?}"));
            }
            let obj = val.as_object().unwrap();
            if obj.contains_key("success") {
                return Err("Result must not be wrapped in success object".to_string());
            }
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            if !(has_steps || has_tool_output) {
                return Err("Expected 'steps' or 'result'/'formatted' in result".to_string());
            }
            Ok(())
        },
    )
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
    load_env();
    require_fixture_runtime_types();

    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    let agent_dir = agent_fixture("stream-baml-tool");
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = r#"
        async function getCalcPlan(args) {
            return await ChooseCalcTool({
                user_message: args.message || "compute 2+3"
            });
        }
        globalThis.getCalcPlan = getCalcPlan;
    "#;
    let effect_bus = Arc::new(BusWithEffects::new());
    baml_manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_init_js(agent_code)
        .with_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>)
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    wire_bridge_effect_liveness(&agent, effect_bus).await;
    let bridge_handle = agent.bridge();
    let scope = InvocationScope::synthetic_message(agent.agent_id().clone());

    let result = run_live_llm_with_retry_validate(
        "ChooseCalcTool contract js",
        3,
        std::time::Duration::from_secs(120),
        |_| {
            let scope = scope.clone();
            let bridge_handle = bridge_handle.clone();
            async move {
                let mut bridge = bridge_handle.lock().await;
                bridge
                    .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
                    .await
            }
        },
        |val| {
            if !val.is_object() {
                return Err(format!("Expected object result, got: {val:?}"));
            }
            let obj = val.as_object().unwrap();
            if obj.contains_key("success") {
                return Err("Result must not contain 'success' wrapper".to_string());
            }
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            if !(has_steps || has_tool_output) {
                return Err("Expected 'steps' or 'result'/'formatted' in result".to_string());
            }
            Ok(())
        },
    )
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
    load_env();
    require_fixture_runtime_types();

    let agent_dir = agent_fixture("stream-baml-tool");
    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();
    baml_manager.register_tool(CalculatorTool).await.unwrap();
    let agent_code = r#"
        async function getCalcPlan(args) {
            return await ChooseCalcTool({
                user_message: args.message || "compute 2+3"
            });
        }
        globalThis.getCalcPlan = getCalcPlan;
    "#;
    let effect_bus = Arc::new(BusWithEffects::new());
    baml_manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_init_js(agent_code)
        .with_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>)
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    wire_bridge_effect_liveness(&agent, effect_bus).await;
    let bridge_handle = agent.bridge();
    let scope = InvocationScope::synthetic_message(agent.agent_id().clone());

    let result = run_live_llm_with_retry_validate(
        "ChooseCalcTool contract api",
        3,
        std::time::Duration::from_secs(120),
        |_| {
            let scope = scope.clone();
            let bridge_handle = bridge_handle.clone();
            async move {
                let mut bridge = bridge_handle.lock().await;
                bridge
                    .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
                    .await
            }
        },
        |val| {
            if !val.is_object() {
                return Err(format!("Expected object result, got: {val:?}"));
            }
            let obj = val.as_object().unwrap();
            if obj.contains_key("success") {
                return Err("Result must not contain 'success' wrapper".to_string());
            }
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            if !(has_steps || has_tool_output) {
                return Err("Expected 'steps' or 'result'/'formatted' in result".to_string());
            }
            Ok(())
        },
    )
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
    load_env();
    require_fixture_runtime_types();

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
                    user_message: args.message || "compute 2+3"
                });
            }
            globalThis.getCalcPlan = getCalcPlan;
        "#,
        );
    }
    let effect_bus = Arc::new(BusWithEffects::new());
    runtime_manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    let agent = A2aAgent::builder()
        .with_runtime_manager(runtime_manager)
        .with_init_js(agent_code)
        .with_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>)
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    wire_bridge_effect_liveness(&agent, effect_bus).await;
    let bridge_handle = agent.bridge();
    let scope = InvocationScope::synthetic_message(agent.agent_id().clone());

    let result = run_live_llm_with_retry_validate(
        "ChooseCalcTool contract js",
        3,
        std::time::Duration::from_secs(120),
        |_| {
            let scope = scope.clone();
            let bridge_handle = bridge_handle.clone();
            async move {
                let mut bridge = bridge_handle.lock().await;
                bridge
                    .invoke_js_function(&scope, "getCalcPlan", json!({"message": "compute 2+3"}))
                    .await
            }
        },
        |val| {
            if !val.is_object() {
                return Err(format!("Expected object result, got: {val:?}"));
            }
            let obj = val.as_object().unwrap();
            if obj.contains_key("success") {
                return Err("Result must not contain 'success' wrapper".to_string());
            }
            let has_steps = val.get("steps").and_then(|v| v.as_array()).is_some();
            let has_tool_output = obj.contains_key("result") || obj.contains_key("formatted");
            if !(has_steps || has_tool_output) {
                return Err("Expected 'steps' or 'result'/'formatted' in result".to_string());
            }
            Ok(())
        },
    )
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
