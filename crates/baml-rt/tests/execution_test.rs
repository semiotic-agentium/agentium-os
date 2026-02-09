//! Tests for BAML function execution

use serde_json::json;
use test_support::common::{ensure_baml_src_exists, setup_baml_runtime_manager_default};

#[tokio::test]
async fn test_load_and_execute_simple_greeting() {
    // Load schema from baml_src (compiled directory)
    // TODO: Migrate to use compiled fixtures once we have a better strategy
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();

    // Verify function was discovered
    let functions = manager.list_functions();
    assert!(
        functions.contains(&"SimpleGreeting".to_string()),
        "Should discover SimpleGreeting function. Found: {:?}",
        functions
    );

    // Execute the function
    // Note: This will make an actual LLM call unless we stub it
    // For now, we expect it to at least attempt execution
    let result = manager
        .invoke_function("SimpleGreeting", json!({"name": "Alice"}))
        .await;

    // Execution should either succeed or fail with a specific error (like missing API key)
    // but should NOT fail with "function not found" or "not implemented"
    match result {
        Ok(value) => {
            // If it succeeds, should return a string
            assert!(value.is_string(), "Result should be a string");
            let response = value.as_str().unwrap();
            assert!(!response.is_empty(), "Response should not be empty");
            println!("Function executed successfully: {}", response);
        }
        Err(e) => {
            // Check error is not "not implemented" or "not found"
            let err_msg = format!("{}", e);
            assert!(
                !err_msg.contains("not yet implemented")
                    && !err_msg.contains("not implemented")
                    && !err_msg.contains("FunctionNotFound"),
                "Should not fail with implementation errors. Error: {}",
                err_msg
            );
            // Other errors (like missing API keys) are acceptable for now
            println!(
                "Function execution failed (likely API/config issue): {}",
                err_msg
            );
        }
    }
}

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
        "Should discover SimpleGreeting function. Found: {:?}",
        functions
    );
}

#[tokio::test]
async fn test_invoke_nonexistent_function_fails() {
    // Load schema from baml_src directory (not a specific file)
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();

    // Try to invoke a function that doesn't exist
    let result = manager
        .invoke_function("NonexistentFunction", json!({}))
        .await;

    assert!(result.is_err(), "Should fail for nonexistent function");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("FunctionNotFound") || err_msg.contains("not found"),
        "Should return FunctionNotFound error. Got: {}",
        err_msg
    );
}
