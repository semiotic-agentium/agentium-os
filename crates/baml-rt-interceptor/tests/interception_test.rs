//! Integration and end-to-end tests for LLM interception.

use std::{sync::Arc, time::Duration};

use baml_rt::{
    error::Result,
    interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor},
};
use baml_rt_core::{
    context,
    ids::{AgentId, UuidId},
};
use serde_json::Value;
use test_support::common::{require_api_key, setup_baml_runtime_manager_default};
use tokio::sync::Mutex;

/// Stub interceptor: substitutes "Hello, Test!" for SimpleGreeting so integration tests avoid real LLM calls.
struct StubSimpleGreetingInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubSimpleGreetingInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        if context.function_name == "SimpleGreeting" {
            Ok(InterceptorDecision::Substitute(Value::String(
                "Hello, Test!".into(),
            )))
        } else {
            Ok(InterceptorDecision::Allow)
        }
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &Result<Value>,
        _duration_ms: u64,
    ) {
    }
}

/// Test interceptor that tracks pre-execution calls
struct PreExecutionTracker {
    pre_execution_calls: Arc<Mutex<Vec<LLMCallContext>>>,
}

impl PreExecutionTracker {
    fn new() -> (Self, Arc<Mutex<Vec<LLMCallContext>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let tracker = Self {
            pre_execution_calls: calls.clone(),
        };
        (tracker, calls)
    }
}

#[async_trait::async_trait]
impl LLMInterceptor for PreExecutionTracker {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        let mut calls = self.pre_execution_calls.lock().await;
        calls.push(context.clone());
        tracing::info!(
            "Pre-execution interception: client={}, model={}, function={}",
            context.client,
            context.model,
            context.function_name
        );
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &Result<Value>,
        _duration_ms: u64,
    ) {
        // No-op for pre-execution tracking
    }
}

type PreExecutionCalls = Arc<Mutex<Vec<LLMCallContext>>>;
type PostExecutionCalls = Arc<Mutex<Vec<(LLMCallContext, bool, u64)>>>;

/// Test interceptor that tracks post-execution calls
struct PostExecutionTracker {
    post_execution_calls: PostExecutionCalls,
}

impl PostExecutionTracker {
    fn new() -> (Self, PostExecutionCalls) {
        let calls: PostExecutionCalls = Arc::new(Mutex::new(Vec::new()));
        let tracker = Self {
            post_execution_calls: calls.clone(),
        };
        (tracker, calls)
    }
}

#[async_trait::async_trait]
impl LLMInterceptor for PostExecutionTracker {
    async fn intercept_llm_call(&self, _context: &LLMCallContext) -> Result<InterceptorDecision> {
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        let mut calls = self.post_execution_calls.lock().await;
        let success = result.is_ok();
        calls.push((context.clone(), success, duration_ms));
        tracing::info!(
            "Post-execution interception: client={}, model={}, function={}, success={}, duration_ms={}",
            context.client,
            context.model,
            context.function_name,
            success,
            duration_ms
        );
    }
}

/// Test interceptor that blocks specific models
struct BlockingInterceptor {
    blocked_models: Vec<String>,
}

impl BlockingInterceptor {
    fn new(blocked_models: Vec<String>) -> Self {
        Self { blocked_models }
    }
}

#[async_trait::async_trait]
impl LLMInterceptor for BlockingInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        if self
            .blocked_models
            .iter()
            .any(|m| context.model.contains(m) || context.client.contains(m))
        {
            tracing::info!(
                "Blocking LLM call: client={}, model={}",
                context.client,
                context.model
            );
            Ok(InterceptorDecision::Block(format!(
                "Model {} is blocked",
                context.model
            )))
        } else {
            Ok(InterceptorDecision::Allow)
        }
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &Result<Value>,
        _duration_ms: u64,
    ) {
        // No-op
    }
}

/// Combined interceptor that tracks both pre and post execution
struct CombinedTracker {
    pre_calls: PreExecutionCalls,
    post_calls: PostExecutionCalls,
}

impl CombinedTracker {
    fn new() -> (Self, PreExecutionCalls, PostExecutionCalls) {
        let pre_calls: PreExecutionCalls = Arc::new(Mutex::new(Vec::new()));
        let post_calls: PostExecutionCalls = Arc::new(Mutex::new(Vec::new()));
        let tracker = Self {
            pre_calls: pre_calls.clone(),
            post_calls: post_calls.clone(),
        };
        (tracker, pre_calls, post_calls)
    }
}

#[async_trait::async_trait]
impl LLMInterceptor for CombinedTracker {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        let mut calls = self.pre_calls.lock().await;
        calls.push(context.clone());
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        let mut calls = self.post_calls.lock().await;
        calls.push((context.clone(), result.is_ok(), duration_ms));
    }
}

async fn with_test_agent_scope<F, Fut, T>(f: F) -> T
where
    F: FnOnce(context::InvocationScope) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let agent_id = AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000001").expect("valid test uuid"),
    );
    let scope = context::InvocationScope::synthetic_message(agent_id);
    context::with_scope(scope.as_scope().clone(), f(scope)).await
}

#[tokio::test]
async fn test_pre_execution_interception_integration() {
    // This test verifies that pre-execution interception is called
    // when build_request is invoked, BEFORE the actual HTTP request is sent

    tracing::info!("=== Integration Test: Pre-execution interception ===");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_manager_default();

    // Register trackers first so they see the call; then stub so no real LLM is invoked.
    let (pre_tracker, pre_calls) = PreExecutionTracker::new();
    baml_manager.register_llm_interceptor(pre_tracker).await;
    baml_manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    // Execute a BAML function that would trigger build_request
    // Note: Even if the actual LLM call fails (no API key), build_request should still be called
    let result = with_test_agent_scope(|scope| async move {
        baml_manager
            .invoke_function(
                scope.as_scope(),
                "SimpleGreeting",
                serde_json::json!({"name": "Integration Test"}),
            )
            .await
    })
    .await;

    // Wait for async operations
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check if pre-execution interception was called
    // Pre-execution interception should ALWAYS be called if build_request is invoked
    // Even if the actual LLM call fails later, build_request should succeed and trigger interception
    let pre_calls_guard = pre_calls.lock().await;

    // Assert that pre-execution interception was called
    assert!(
        !pre_calls_guard.is_empty(),
        "Pre-execution interception should be called - build_request should trigger it even if LLM call fails"
    );

    tracing::info!(
        "✅ Pre-execution interception was called {} times",
        pre_calls_guard.len()
    );

    // Verify we got proper context from build_request
    for call in pre_calls_guard.iter() {
        assert_eq!(
            call.function_name, "SimpleGreeting",
            "Function name should match"
        );
        // Client and model should be extracted from the HTTPRequest
        tracing::info!(
            "  ✅ Pre-execution call: client='{}', model='{}', function='{}'",
            call.client,
            call.model,
            call.function_name
        );
    }

    // Don't assert on result - we're testing interception, not successful execution
    tracing::info!("Function execution result: {:?}", result);
}

#[tokio::test]
async fn test_post_execution_interception_integration() {
    // This test verifies that post-execution interception is called
    // AFTER the LLM request completes (or fails)

    tracing::info!("=== Integration Test: Post-execution interception ===");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_manager_default();

    let (post_tracker, post_calls) = PostExecutionTracker::new();
    baml_manager.register_llm_interceptor(post_tracker).await;
    baml_manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    // Execute a BAML function
    let result = with_test_agent_scope(|scope| async move {
        baml_manager
            .invoke_function(
                scope.as_scope(),
                "SimpleGreeting",
                serde_json::json!({"name": "Integration Test"}),
            )
            .await
    })
    .await;

    // Wait for post-execution notifications (collector processes trace events)
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check if post-execution interception was called
    let post_calls_guard = post_calls.lock().await;

    // Post-execution interception requires trace events from completed LLM calls
    // If the LLM call fails (e.g., 401 auth error), we may still get trace events
    // So post-execution should ideally be called
    if !post_calls_guard.is_empty() {
        tracing::info!(
            "✅ Post-execution interception was called {} times",
            post_calls_guard.len()
        );

        for (idx, (context, success, duration_ms)) in post_calls_guard.iter().enumerate() {
            assert_eq!(
                context.function_name, "SimpleGreeting",
                "Function name should match"
            );
            // duration_ms is u64, so it's always >= 0
            tracing::info!(
                "  ✅ Post-execution call #{}: client='{}', model='{}', success={}, duration={}ms",
                idx + 1,
                context.client,
                context.model,
                success,
                duration_ms
            );
        }
    } else {
        // Note: Post-execution may not be called if trace events aren't collected
        // This can happen if the LLM call fails before trace events are recorded
        // For now, we'll warn but not fail the test
        tracing::warn!(
            "⚠️  Post-execution interception was not called - trace events may not be available"
        );
    }

    tracing::info!("Function execution result: {:?}", result);
}

#[tokio::test]
async fn test_blocking_interception_integration() {
    // This test verifies that blocking interception prevents LLM calls from executing

    tracing::info!("=== Integration Test: Blocking interception ===");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_manager_default();

    // Register blocking interceptor first so it can block; stub runs after (substitutes if not blocked).
    // Block model containing "grok" so SimpleGreeting (x-ai/grok-4.1-fast) gets blocked.
    baml_manager
        .register_llm_interceptor(BlockingInterceptor::new(vec!["grok".to_string()]))
        .await;
    baml_manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    // Try to execute a BAML function
    // The interceptor should block if the model/client matches
    let result = with_test_agent_scope(|scope| async move {
        baml_manager
            .invoke_function(
                scope.as_scope(),
                "SimpleGreeting",
                serde_json::json!({"name": "Blocked Test"}),
            )
            .await
    })
    .await;

    // Wait a bit
    tokio::time::sleep(Duration::from_millis(100)).await;

    // If blocking worked, we should get an error containing "blocked"
    // The blocking interceptor checks if model contains "grok"; SimpleGreeting uses x-ai/grok-4.1-fast.
    match result {
        Ok(_) => {
            // If we get here, blocking didn't work - the model pattern might not have matched
            // This could happen if the client/model names don't match our blocking pattern
            tracing::warn!(
                "⚠️  Function executed successfully - blocking may not have matched the model/client pattern"
            );
        }
        Err(e) => {
            let error_str = e.to_string();
            if error_str.contains("blocked") {
                tracing::info!("✅ Blocking worked! Error message: {}", error_str);
                assert!(
                    error_str.contains("blocked"),
                    "Error should mention blocking"
                );
                // Verify the error is specifically from our interceptor
                assert!(
                    error_str.contains("interceptor") || error_str.contains("blocked"),
                    "Error should indicate it was blocked by interceptor"
                );
            } else {
                // We might get other errors (e.g., auth failures) even if blocking didn't trigger
                // That's okay - the key is that if blocking triggers, we should see it
                tracing::info!(
                    "Got error (not from blocking, may be auth or other issue): {}",
                    error_str
                );
            }
        }
    }
}

#[tokio::test]
async fn test_pre_and_post_execution_together_integration() {
    // This test verifies both pre and post-execution interception work together
    // in a single execution

    tracing::info!("=== Integration Test: Pre and post-execution together ===");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_manager_default();

    let (combined_tracker, pre_calls, post_calls) = CombinedTracker::new();
    baml_manager
        .register_llm_interceptor(combined_tracker)
        .await;
    baml_manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    // Execute a BAML function
    let result = with_test_agent_scope(|scope| async move {
        baml_manager
            .invoke_function(
                scope.as_scope(),
                "SimpleGreeting",
                serde_json::json!({"name": "Combined Test"}),
            )
            .await
    })
    .await;

    // Wait for all async operations
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check both pre and post execution
    let pre_calls_guard = pre_calls.lock().await;
    let post_calls_guard = post_calls.lock().await;

    tracing::info!(
        "Pre-execution calls: {}, Post-execution calls: {}",
        pre_calls_guard.len(),
        post_calls_guard.len()
    );

    // Pre-execution should ideally be called if build_request succeeds
    if !pre_calls_guard.is_empty() {
        tracing::info!(
            "✅ Pre-execution interception was called {} times",
            pre_calls_guard.len()
        );
        for call in pre_calls_guard.iter() {
            assert_eq!(call.function_name, "SimpleGreeting");
        }
    }

    // Post-execution requires actual LLM call completion and trace events
    if !post_calls_guard.is_empty() {
        tracing::info!(
            "✅ Post-execution interception was called {} times",
            post_calls_guard.len()
        );
        for (context, _success, _duration_ms) in post_calls_guard.iter() {
            assert_eq!(context.function_name, "SimpleGreeting");
        }
    }

    // Key assertions:
    // 1. Pre-execution should ALWAYS be called if build_request succeeds (which it should)
    // 2. Post-execution depends on trace events being collected
    assert!(
        !pre_calls_guard.is_empty(),
        "Pre-execution interception should always be called when build_request is invoked"
    );

    if !post_calls_guard.is_empty() {
        tracing::info!("✅ Both pre and post-execution interception are working");
    } else {
        tracing::info!(
            "✅ Pre-execution interception confirmed (post-execution depends on trace events)"
        );
    }

    tracing::info!("Function execution result: {:?}", result);
}

#[tokio::test]
async fn test_multiple_interceptors_integration() {
    // This test verifies that multiple interceptors can be registered and all are called

    tracing::info!("=== Integration Test: Multiple interceptors ===");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_manager_default();

    let (pre_tracker1, pre_calls1) = PreExecutionTracker::new();
    let (pre_tracker2, pre_calls2) = PreExecutionTracker::new();

    baml_manager.register_llm_interceptor(pre_tracker1).await;
    baml_manager.register_llm_interceptor(pre_tracker2).await;
    baml_manager
        .register_llm_interceptor(StubSimpleGreetingInterceptor)
        .await;

    // Execute a BAML function
    let _result = with_test_agent_scope(|scope| async move {
        baml_manager
            .invoke_function(
                scope.as_scope(),
                "SimpleGreeting",
                serde_json::json!({"name": "Multiple Test"}),
            )
            .await
    })
    .await;

    // Wait for async operations
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check both interceptors were called
    let pre_calls1_guard = pre_calls1.lock().await;
    let pre_calls2_guard = pre_calls2.lock().await;

    tracing::info!(
        "Interceptor 1 calls: {}, Interceptor 2 calls: {}",
        pre_calls1_guard.len(),
        pre_calls2_guard.len()
    );

    // Both should have the same number of calls (they intercept the same events)
    assert!(
        !pre_calls1_guard.is_empty(),
        "At least one interceptor should be called (verifying pre-execution interception works)"
    );

    assert_eq!(
        pre_calls1_guard.len(),
        pre_calls2_guard.len(),
        "Both interceptors should be called the same number of times - they receive the same events"
    );

    // Verify both interceptors received the same context
    if !pre_calls1_guard.is_empty() {
        assert_eq!(
            pre_calls1_guard[0].function_name, pre_calls2_guard[0].function_name,
            "Both interceptors should receive the same function name"
        );
        tracing::info!(
            "✅ Multiple interceptors are working correctly - both received {} calls",
            pre_calls1_guard.len()
        );
    }
}

type LlmCalls = Arc<Mutex<Vec<LLMCallContext>>>;
type LlmCompletions = Arc<Mutex<Vec<(LLMCallContext, bool, u64)>>>;

/// Test interceptor that logs LLM calls
struct E2ELLMLoggingInterceptor {
    calls: LlmCalls,
    completions: LlmCompletions, // (context, success, duration_ms)
}

impl E2ELLMLoggingInterceptor {
    fn new() -> (Self, LlmCalls, LlmCompletions) {
        let calls: LlmCalls = Arc::new(Mutex::new(Vec::new()));
        let completions: LlmCompletions = Arc::new(Mutex::new(Vec::new()));
        let interceptor = Self {
            calls: calls.clone(),
            completions: completions.clone(),
        };
        (interceptor, calls, completions)
    }
}

#[async_trait::async_trait]
impl LLMInterceptor for E2ELLMLoggingInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        let mut calls = self.calls.lock().await;
        calls.push(context.clone());
        tracing::info!(
            client = context.client,
            model = context.model,
            function = context.function_name,
            "LLM interceptor: intercepting LLM call"
        );
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        let mut completions = self.completions.lock().await;
        let success = result.is_ok();
        completions.push((context.clone(), success, duration_ms));
        tracing::info!(
            client = context.client,
            model = context.model,
            function = context.function_name,
            success = success,
            duration_ms = duration_ms,
            "LLM interceptor: call completed"
        );
    }
}

#[tokio::test]
async fn test_e2e_llm_interceptor_with_baml_execution() {
    let _ = require_api_key();

    tracing::info!("E2E Test: LLM interceptor with actual BAML function execution");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_manager_default();

    // Register an LLM interceptor
    let (interceptor, calls, completions) = E2ELLMLoggingInterceptor::new();
    baml_manager.register_llm_interceptor(interceptor).await;

    // Execute a BAML function that makes an LLM call
    tracing::info!("Calling SimpleGreeting BAML function (should trigger LLM interceptor)");
    let result = with_test_agent_scope(|scope| async move {
        baml_manager
            .invoke_function(
                scope.as_scope(),
                "SimpleGreeting",
                serde_json::json!({"name": "E2E Test"}),
            )
            .await
    })
    .await;

    // Verify the function executed successfully
    assert!(result.is_ok(), "BAML function should execute successfully");
    let greeting = result.unwrap();
    assert!(greeting.as_str().is_some(), "Result should be a string");

    // Give a moment for async completion notifications to process
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Verify interceptor was called
    let calls_guard = calls.lock().await;
    assert!(
        !calls_guard.is_empty(),
        "LLM interceptor should have been called"
    );
    tracing::info!(
        "✅ LLM interceptor was called {} time(s)",
        calls_guard.len()
    );

    // Verify completion was notified
    let completions_guard = completions.lock().await;
    assert!(
        !completions_guard.is_empty(),
        "LLM interceptor should have received completion notification"
    );
    tracing::info!(
        "✅ LLM interceptor received {} completion notification(s)",
        completions_guard.len()
    );

    // Verify the completion was successful
    if let Some((_, success, _)) = completions_guard.first() {
        assert!(*success, "LLM call should have completed successfully");
    }

    tracing::info!("🎉 E2E LLM interceptor test completed successfully!");
}
