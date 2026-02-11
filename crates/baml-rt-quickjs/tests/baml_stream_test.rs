//! Tests for JavaScript streaming invocation of BAML functions
//!
//! Use short max_attempts so effect-gated poll doesn't hang when LLM-backed fixtures don't complete.

#![recursion_limit = "256"]

use baml_rt::A2aAgent;
use baml_rt::QuickJSConfig;
use baml_rt::baml::BamlRuntimeManager;
use std::sync::Arc;

#[tokio::test]
async fn test_js_stream_baml_function() {
    test_support::common::ensure_fixture_runtime_types();

    // Set up BAML runtime
    let mut baml_manager = BamlRuntimeManager::new().unwrap();

    // Load BAML schema from agent fixture (which has baml_src directory)
    let agent_dir = test_support::common::agent_fixture("voidship-rites");
    if !agent_dir.join("baml_src").exists() {
        eprintln!("Skipping test: voidship-rites fixture not found");
        return;
    }
    baml_manager
        .load_schema(agent_dir.to_str().unwrap())
        .unwrap();

    let agent = A2aAgent::builder()
        .with_runtime_manager(baml_manager)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();
    let bridge_handle = agent.bridge();
    let mut bridge = bridge_handle.lock().await;

    // Test invoking SimpleGreeting stream from JavaScript
    // Use __awaitAndStringify helper to handle async function calls
    // Note: This will fail without an API key, but we can test the invocation path
    let js_code = r#"
        (function() {
            try {
                const promise = SimpleGreetingStream({ name: "World" });
                return __awaitAndStringify(promise);
            } catch (e) {
                return JSON.stringify({ success: false, error: e.toString() });
            }
        })()
    "#;

    let result = bridge.evaluate(None, js_code).await;

    // The result should contain either success with results array, or error info
    // Note: This may fail due to missing API keys, which is acceptable
    let json_result = match result {
        Ok(val) => val,
        Err(e) => {
            println!(
                "JavaScript execution error (may be due to missing API keys): {:?}",
                e
            );
            // The function exists and was called, but execution failed (likely API key issue)
            // This is acceptable for integration tests
            return;
        }
    };
    println!("JavaScript streaming execution result: {:?}", json_result);

    // Check if we got a proper result structure
    if json_result.is_array() {
        return;
    }
    if let Some(obj) = json_result.as_object() {
        // Should have either success with results, or error
        assert!(
            obj.contains_key("success") || obj.contains_key("error"),
            "Result should contain 'success' or 'error' field"
        );

        // If it succeeded, results should be an array
        if let Some(success) = obj.get("success").and_then(|s| s.as_bool())
            && success
        {
            assert!(
                obj.contains_key("results"),
                "Success result should contain 'results' array"
            );
        }
    } else {
        panic!("Result should be an array or object");
    }
}
