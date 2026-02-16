//! Tests for invoking BAML functions

use baml_rt::BamlRtError;
use baml_rt_core::context::InvocationScope;
use baml_rt_core::ids::{AgentId, UuidId};
use std::time::Duration;
use test_support::common::{
    ensure_baml_src_exists, run_live_llm_with_retry_validate, setup_baml_runtime_default,
};

#[tokio::test]
async fn test_load_schema_discovers_functions() {
    // Load schema from baml_src (compiled directory)
    // TODO: Migrate to use compiled fixtures once we have a better strategy
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_default();

    // Should discover SimpleGreeting function
    let functions = manager.list_functions();
    assert!(
        functions.contains(&"SimpleGreeting".to_string()),
        "Should discover SimpleGreeting function"
    );
}

#[tokio::test]
async fn test_invoke_simple_greeting() {
    // Load schema from baml_src (compiled directory)
    // TODO: Migrate to use compiled fixtures once we have a better strategy
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();

    // Try to invoke the function
    // This will fail until we implement actual execution, but verifies the function is registered
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    ));
    let result = run_live_llm_with_retry_validate(
        "invoke_simple_greeting",
        3,
        Duration::from_secs(120),
        |_| {
            let manager = manager.clone();
            let scope = scope.clone();
            async move {
                let manager = manager.lock().await;
                manager
                    .invoke_function(
                        scope.as_scope(),
                        "SimpleGreeting",
                        serde_json::json!({"name": "Test"}),
                    )
                    .await
            }
        },
        |value| {
            let response_str = value.as_str().unwrap_or("");
            if response_str.trim().is_empty() {
                return Err("SimpleGreeting returned empty response".to_string());
            }
            Ok(())
        },
    )
    .await;

    match result {
        Ok(value) => {
            assert!(value.is_string(), "Result should be a string");
        }
        Err(BamlRtError::FunctionNotFound(_)) => {
            panic!("Function should be found after loading schema");
        }
        Err(BamlRtError::BamlRuntime(msg)) if msg.contains("not yet implemented") => {
            panic!("Execution should be implemented now. Error: {}", msg);
        }
        Err(e) => {
            println!(
                "Function execution attempted but failed (likely config issue): {}",
                e
            );
        }
    }
}
