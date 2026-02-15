//! Tests for BAML function execution

use baml_rt_core::context::InvocationScope;
use baml_rt_core::ids::{AgentId, UuidId};
use serde_json::json;
use std::time::Duration;
use test_support::common::{
    ensure_baml_src_exists, require_api_key, run_live_llm_with_retry,
    setup_baml_runtime_manager_default,
};

#[tokio::test]
async fn test_load_and_execute_simple_greeting() {
    // Load schema from baml_src (compiled directory)
    // TODO: Migrate to use compiled fixtures once we have a better strategy
    if !ensure_baml_src_exists() {
        return;
    }
    let _ = require_api_key();
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
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000e1").unwrap(),
    ));
    let value = run_live_llm_with_retry(
        "SimpleGreeting execute",
        3,
        Duration::from_secs(120),
        |_| async {
            manager
                .invoke_function(scope.as_scope(), "SimpleGreeting", json!({"name": "Alice"}))
                .await
        },
    )
    .await
    .expect("BAML function should execute successfully");

    assert!(value.is_string(), "Result should be a string");
    let response = value.as_str().unwrap_or("").trim().to_string();
    assert!(!response.is_empty(), "Response should not be empty");
}

#[tokio::test]
async fn test_invoke_nonexistent_function_fails() {
    // Load schema from baml_src directory (not a specific file)
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();

    // Try to invoke a function that doesn't exist
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000e2").unwrap(),
    ));
    let result = manager
        .invoke_function(scope.as_scope(), "NonexistentFunction", json!({}))
        .await;

    assert!(result.is_err(), "Should fail for nonexistent function");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("FunctionNotFound") || err_msg.contains("not found"),
        "Should return FunctionNotFound error. Got: {}",
        err_msg
    );
}
