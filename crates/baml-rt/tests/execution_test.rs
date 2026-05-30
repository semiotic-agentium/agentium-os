// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tests for BAML function execution.
//!
//! `test_load_and_execute_simple_greeting` uses an LLM interceptor stub so it does not call the real LLM.
//!
//! Other tests that could be made non-LLM-dependent (stub via LLM interceptor):
//! - **contracts_test**: `test_baml_function_returns_actual_result`, `test_js_function_invocation_returns_actual_result`,
//!   `test_invoke_function_api_contract`, `test_loaded_agent_invoke_function_contract` — all call ChooseCalcTool/getCalcPlan;
//!   stub could return a valid session plan (steps with Open/Send/Next/Finish).
//! - **tool_calling_test**: `test_e2e_voidship_baml_tool_calling`, `test_e2e_voidship_baml_tool_calling_concurrent` —
//!   same pattern (ChooseCalcTool); stub would allow testing tool execution and concurrency without API key.
//! - **llm_test**: `test_e2e_simple_greeting_with_llm`, `test_e2e_streaming_greeting` — explicitly LLM e2e; keep behind
//!   `llm-tests` feature or leave as live LLM.

use baml_rt::interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor};
use baml_rt_core::{
    context::InvocationScope,
    ids::{AgentId, UuidId},
};
use serde_json::json;
use test_support::common::{ensure_baml_src_exists, setup_baml_runtime_manager_default};

/// Stub interceptor: returns a canned string for SimpleGreeting so this test avoids real LLM calls.
struct StubSimpleGreetingInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubSimpleGreetingInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_id.prompt_name().as_str() == "SimpleGreeting" {
            Ok(InterceptorDecision::Substitute(serde_json::Value::String(
                "Hello, Test!".to_string(),
            )))
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
async fn test_load_and_execute_simple_greeting() {
    // Load schema from baml_src (compiled directory)
    // TODO: Migrate to use compiled fixtures once we have a better strategy
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();
    manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    // Verify function was discovered
    let functions = manager.list_functions();
    assert!(
        functions.contains(&"SimpleGreeting".to_string()),
        "Should discover SimpleGreeting function. Found: {:?}",
        functions
    );

    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000e1").unwrap(),
    ));
    let result = manager
        .invoke_function(scope.as_scope(), "SimpleGreeting", json!({"name": "Alice"}))
        .await;

    match result {
        Ok(value) => {
            assert!(value.is_string(), "Result should be a string");
            let response = value.as_str().unwrap();
            assert!(!response.is_empty(), "Response should not be empty");
            assert_eq!(
                response, "Hello, Test!",
                "Stub should return canned greeting"
            );
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            assert!(
                !err_msg.contains("not yet implemented")
                    && !err_msg.contains("not implemented")
                    && !err_msg.contains("FunctionNotFound"),
                "Should not fail with implementation errors. Error: {}",
                err_msg
            );
            panic!("Invoke should succeed with stub interceptor: {}", err_msg);
        }
    }
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
