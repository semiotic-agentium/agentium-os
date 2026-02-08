//! Contract tests for BAML function invocation results
//!
//! These tests assert on the actual structure and content of results,
//! ensuring the contract between JavaScript/BAML functions and the runtime is correct.
//!
//! Use short max_attempts_ms so tests fail fast when LLM-backed fixtures (e.g. VoidshipGreeting)
//! don't complete (e.g. missing API key); otherwise the effect-gated poller would wait 30 minutes.

use std::fs;
use std::sync::Arc;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::A2aAgent;
use baml_rt::QuickJSConfig;
use baml_rt_core::context::InvocationScope;
use serde_json::json;

use test_support::common::agent_fixture;

/// QuickJS config for tests: short max_attempts so effect-gated poll doesn't hang (LLM fixtures).
fn test_quickjs_config() -> QuickJSConfig {
    QuickJSConfig::new().with_max_attempts_ms(Some(15_000)) // 15s instead of 30 min
}

#[tokio::test]
async fn test_baml_function_returns_string_result() {
    // Contract: When a BAML function returns a string, invoke_function should return that string
    // (not wrapped in {"success": true} or any other wrapper)
    
    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    let agent_dir = agent_fixture("voidship-rites");
    baml_manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(test_quickjs_config())
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // Invoke VoidshipGreeting which should return a string greeting
    let result = bridge
        .invoke_function("VoidshipGreeting", json!({"name": "TestUser"}))
        .await;
    
    // Contract assertion: Result should be a string (the greeting), not a wrapper object
    match result {
        Ok(val) => {
            // Should be a string, not an object with "success" field
            assert!(
                val.is_string(),
                "Expected string result, got: {:?}. The function should return the actual greeting string.",
                val
            );
            
            let greeting = val.as_str().unwrap();
            assert!(
                !greeting.trim().is_empty(),
                "Expected non-empty greeting string, got: '{}'",
                greeting
            );
        }
        Err(e) => {
            panic!(
                "Unexpected error: {}. Contract violation: function should return string result.",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_js_function_invocation_returns_actual_result() {
    // Contract: When invoking a JavaScript function that calls BAML, 
    // the result should be the actual BAML result, not a success wrapper
    
    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    let agent_dir = agent_fixture("voidship-rites");
    baml_manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    let agent_code = r#"
        async function riteBlessing(args) {
            return await VoidshipGreeting({ name: args.name });
        }
        globalThis.riteBlessing = riteBlessing;
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

    // Invoke riteBlessing - this should return the actual greeting string
    let result = bridge
        .invoke_js_function(&scope, "riteBlessing", json!({"name": "ContractTest"}))
        .await;
    
    // Contract assertion: Should return the actual greeting string, not {"success": true}
    match result {
        Ok(val) => {
            // MUST be a string (the greeting)
            assert!(
                val.is_string(),
                "CONTRACT VIOLATION: Expected string result from riteBlessing, got: {:?}. Actual result must be returned, not wrapped in success object.",
                val
            );
            
            // MUST NOT be a success wrapper
            if let Some(obj) = val.as_object()
                && obj.contains_key("success")
            {
                panic!(
                    "CONTRACT VIOLATION: Result contains 'success' field. Expected actual result (string), got: {:?}",
                    val
                );
            }
            
            let greeting = val.as_str().unwrap();
            assert!(
                !greeting.trim().is_empty(),
                "Expected non-empty greeting string, got: '{}'",
                greeting
            );
        }
        Err(e) => {
            panic!(
                "CONTRACT VIOLATION: Unexpected error: {}. Function should return string result.",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_invoke_function_api_contract() {
    // Contract: The invoke_function API (from baml-agent-builder) should return the actual function result,
    // not wrapped in any success object
    
    let agent_dir = agent_fixture("voidship-rites");
    let mut baml_manager = BamlRuntimeManager::new().unwrap();
    baml_manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    let agent_code = r#"
        async function riteBlessing(args) {
            return await VoidshipGreeting({ name: args.name });
        }
        globalThis.riteBlessing = riteBlessing;
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
        .invoke_js_function(&scope, "riteBlessing", json!({"name": "APIContractTest"}))
        .await;
    
    // Contract assertion: Result MUST be the actual string, not wrapped
    match result {
        Ok(val) => {
            // CONTRACT: Must be a string (the greeting)
            assert!(
                val.is_string(),
                "CONTRACT VIOLATION: invoke_function API must return actual result (string), not wrapper. Got: {:?}",
                val
            );
            
            // CONTRACT: Must NOT contain "success" field
            if let Some(obj) = val.as_object()
                && obj.get("success").is_some()
            {
                panic!(
                    "CONTRACT VIOLATION: Result contains 'success' field: {:?}. API must return actual result directly.",
                    val
                );
            }
            
            let greeting = val.as_str().unwrap();
            assert!(
                !greeting.trim().is_empty(),
                "Expected non-empty greeting string, got: '{}'",
                greeting
            );
        }
        Err(e) => {
            // Promise resolution failures are contract violations
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
    // Contract: LoadedAgent::invoke_function must return the actual result, not wrapped
    
    // Load agent exactly as load_agent_package does
    let agent_dir = test_support::common::agent_fixture("voidship-rites");
    
    // Extract logic from load_agent_package (the actual code)
    let mut runtime_manager = BamlRuntimeManager::new().unwrap();
    runtime_manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    
    // Load agent JavaScript code (actual pattern from load_agent_package)
    let dist_path = agent_dir.join("dist").join("index.js");
    let mut agent_code = if dist_path.exists() {
        fs::read_to_string(&dist_path).unwrap()
    } else {
        String::new()
    };
    if !agent_code.contains("globalThis.riteBlessing") {
        agent_code.push_str(
            r#"
            async function riteBlessing(args) {
                return await VoidshipGreeting({ name: args.name });
            }
            globalThis.riteBlessing = riteBlessing;
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
    
    // Use the ACTUAL invoke_function logic from LoadedAgent (lines 257-290)
    let result = bridge
        .invoke_js_function(&scope, "riteBlessing", json!({"name": "ContractTest"}))
        .await;
    
    match result {
        Ok(val) => {
            // CONTRACT: Result must be a string (the actual greeting), not {"success": true}
            assert!(
                val.is_string(),
                "CONTRACT VIOLATION: invoke_function must return string result, got: {:?}",
                val
            );
            
            // CONTRACT: Must NOT be a success wrapper
            if let Some(obj) = val.as_object() {
                panic!("CONTRACT VIOLATION: Result is object with 'success': {:?}. Must return actual result.", obj);
            }
            
            let greeting = val.as_str().unwrap();
            assert!(
                !greeting.trim().is_empty(),
                "Expected non-empty greeting string, got: '{}'",
                greeting
            );
        }
        Err(e) => {
            panic!(
                "CONTRACT VIOLATION: Unexpected error: {}. Function should return string result.",
                e
            );
        }
    }
}
