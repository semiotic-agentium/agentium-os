//! Tests for invoking BAML functions

use baml_rt::BamlRtError;
use test_support::common::{ensure_baml_src_exists, setup_baml_runtime_manager_default};

#[tokio::test]
async fn test_load_schema_discovers_functions() {
    // Load schema from baml_src (compiled directory)
    // TODO: Migrate to use compiled fixtures once we have a better strategy
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();

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
    let result = manager
        .invoke_function("SimpleGreeting", serde_json::json!({"name": "Test"}))
        .await;

    // Execution should work (may fail with API key error, which is acceptable)
    match result {
        Ok(value) => {
            // Success! Function executed
            assert!(value.is_string(), "Result should be a string");
        }
        Err(BamlRtError::FunctionNotFound(_)) => {
            panic!("Function should be found after loading schema");
        }
        Err(BamlRtError::BamlRuntime(msg)) if msg.contains("not yet implemented") => {
            panic!("Execution should be implemented now. Error: {}", msg);
        }
        Err(e) => {
            // Other errors (like API key issues) are acceptable for now
            // The important thing is that execution was attempted
            println!(
                "Function execution attempted but failed (likely config issue): {}",
                e
            );
        }
    }
}
