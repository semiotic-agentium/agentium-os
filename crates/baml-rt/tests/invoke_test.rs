//! Tests for invoking BAML functions.
//!
//! `test_invoke_simple_greeting` uses an interceptor stub so it does not call the real LLM.

use baml_rt::BamlRtError;
use baml_rt::interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor};
use baml_rt_core::context::InvocationScope;
use baml_rt_core::ids::{AgentId, UuidId};
use serde_json::Value;
use test_support::common::{ensure_baml_src_exists, setup_baml_runtime_manager_default};

/// Stub interceptor: returns a canned string for SimpleGreeting so tests avoid real LLM calls.
struct StubSimpleGreetingInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubSimpleGreetingInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_name == "SimpleGreeting" {
            Ok(InterceptorDecision::Substitute(Value::String(
                "Hello, Test!".to_string(),
            )))
        } else {
            Ok(InterceptorDecision::Allow)
        }
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &baml_rt_core::Result<Value>,
        _duration_ms: u64,
    ) {
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
        "Should discover SimpleGreeting function"
    );
}

#[tokio::test]
async fn test_invoke_simple_greeting() {
    if !ensure_baml_src_exists() {
        return;
    }
    let manager = setup_baml_runtime_manager_default();
    manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    ));
    let result = manager
        .invoke_function(
            scope.as_scope(),
            "SimpleGreeting",
            serde_json::json!({"name": "Test"}),
        )
        .await;

    match result {
        Ok(value) => {
            assert!(value.is_string(), "Result should be a string");
            assert_eq!(value.as_str(), Some("Hello, Test!"));
        }
        Err(BamlRtError::FunctionNotFound(_)) => {
            panic!("Function should be found after loading schema");
        }
        Err(BamlRtError::BamlRuntime(msg)) if msg.contains("not yet implemented") => {
            panic!("Execution should be implemented now. Error: {}", msg);
        }
        Err(e) => {
            panic!("Invoke should succeed with stub interceptor: {}", e);
        }
    }
}
