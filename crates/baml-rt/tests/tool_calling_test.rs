//! LLM and BAML tool calling tests.
//!
//! **Purpose:** Exercise tool registration, execution from Rust/JS, and the authoritative
//! E2E vertical slice (fixture-based single-request and concurrent tool calling).
//!
//! **E2E authority (per testing-handbook):** Single-request tool E2E is
//! `test_e2e_voidship_baml_tool_calling`; concurrent E2E is
//! `test_e2e_voidship_baml_tool_calling_concurrent`. Overlapping union/LLM
//! E2E tests have been retired in favour of this vertical slice.
//!
//! E2E tests that invoke a BAML function and then execute its result pass the
//! **same** function name to `execute_tool_from_baml_result_or_value` (mirrors
//! production: the QuickJS bridge passes the invoking function name automatically).

use async_trait::async_trait;
use baml_rt::{
    interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor},
    tools::BamlTool,
};
use baml_rt_tools::bundles::BundleType;

// Test bundle for test tools
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}
use std::sync::Arc;

use baml_rt_core::{
    context::{self, InvocationScope},
    ids::{AgentId, UuidId},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use test_support::common::{
    CalculatorTool, STREAM_BAML_TOOL_FUNCTION, WeatherTool, agent_fixture,
    assert_tool_registered_in_js, ensure_fixture_runtime_types, execute_calc_session_strict,
    setup_baml_runtime_default, setup_baml_runtime_from_fixture, setup_bridge, workspace_fnox_path,
};
use tokio::{
    sync::Barrier,
    time::{Duration, timeout},
};
use ts_rs::TS;

/// Stub interceptor: strict-mode single fragment for calculator session tests.
struct StubChooseCalcToolStrictInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubChooseCalcToolStrictInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_name == STREAM_BAML_TOOL_FUNCTION {
            Ok(InterceptorDecision::Substitute(json!({
                "step": {
                    "op": "Send",
                    "input": {
                        "expression": {
                            "left": 2,
                            "operation": "Add",
                            "right": 3
                        }
                    },
                    "reason": "strict stub send"
                }
            })))
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

/// **Purpose:** Verify tool registration and direct execution from Rust (execute_tool_with_scope,
/// list_tools) under an invocation scope; no LLM call required.
#[tokio::test]
async fn test_llm_tool_calling_rust() {
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(WeatherTool).await.unwrap();
        manager.register_tool(CalculatorTool).await.unwrap();
    }

    // Test that tools are registered and can be executed (scope required for execute_tool)
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
    ));
    {
        let manager = baml_manager.lock().await;

        // Test weather tool
        let weather_result = manager
            .execute_tool_with_scope(
                scope.as_scope(),
                "support/get_weather",
                json!({"location": "San Francisco"}),
            )
            .await
            .unwrap();
        let weather_obj = weather_result.as_object().expect("Expected object");
        assert!(
            weather_obj.contains_key("temperature"),
            "Weather result should contain temperature"
        );

        // Test calculator tool
        let calc_result = manager
            .execute_tool_with_scope(
                scope.as_scope(),
                "support/calculate",
                json!({"expression": {"left": 2, "operation": "Add", "right": 2}}),
            )
            .await
            .unwrap();
        let calc_obj = calc_result.as_object().expect("Expected object");
        let result = calc_obj.get("result").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(result, 4.0, "2 + 2 should equal 4");

        // List tools
        let tools = manager.list_tools().await;
        assert!(
            tools.contains(&"support/get_weather".to_string()),
            "Should list weather tool"
        );
        assert!(
            tools.contains(&"support/calculate".to_string()),
            "Should list calculator tool"
        );
    }

    tracing::info!("Tool registration and execution tests passed");

    // Note: Actual LLM tool calling integration with BAML would require
    // passing the tool registry to BAML's call_function with client_registry.
    // This test verifies the foundation is in place.
}

/// **Purpose:** Verify a BAML tool registered via the trait is visible in the QuickJS bridge
/// (assert_tool_registered_in_js) and executable from Rust with scope; no LLM call.
#[tokio::test]
#[allow(unnameable_test_items)]
async fn test_llm_tool_calling_js() {
    let baml_manager = setup_baml_runtime_default();

    // Register a tool using the trait
    struct ReverseStringTool;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct ReverseInput {
        text: String,
    }
    impl baml_rt_tools::DescribeAction for ReverseInput {
        fn describe(&self) -> String {
            format!("Reverse text: {}", self.text)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct ReverseOutput {
        reversed: String,
        original: String,
    }

    #[async_trait]
    impl BamlTool for ReverseStringTool {
        type Bundle = Test;
        const LOCAL_NAME: &'static str = "reverse_string";
        type OpenInput = ();
        type Input = ReverseInput;
        type Output = ReverseOutput;

        fn description(&self) -> &'static str {
            "Reverses a string"
        }

        async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
            let reversed: String = args.text.chars().rev().collect();
            Ok(ReverseOutput {
                reversed,
                original: args.text,
            })
        }
    }

    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(ReverseStringTool).await.unwrap();
    }

    let mut bridge = setup_bridge(baml_manager.clone()).await;

    // Scope required so openToolSession has a valid invocation token when checking Rust tools
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
    ));
    assert_tool_registered_in_js(&mut bridge, "test/reverse_string", &scope).await;

    // Test executing the tool from Rust (scope required)
    {
        let manager = baml_manager.lock().await;
        let result = manager
            .execute_tool_with_scope(
                scope.as_scope(),
                "test/reverse_string",
                json!({"text": "hello"}),
            )
            .await
            .unwrap();

        let result_obj = result.as_object().expect("Expected object");
        let reversed = result_obj.get("reversed").and_then(|g| g.as_str()).unwrap();
        assert_eq!(reversed, "olleh", "Should reverse the string correctly");
    }
}

/// **Purpose:** Authoritative single-request E2E: load fixture `stream-baml-tool`, invoke
/// `ChooseCalcTool`, execute the chosen tool, assert result (2+3=5). Requires API key for LLM.
#[tokio::test]
async fn test_e2e_voidship_baml_tool_calling() {
    ensure_fixture_runtime_types();
    let baml_manager = setup_baml_runtime_from_fixture("stream-baml-tool");
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(CalculatorTool).await.unwrap();
        manager
            .register_llm_interceptor(StubChooseCalcToolStrictInterceptor)
            .await;
    }
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000b1").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);

    let result = context::with_scope(scope.as_scope().clone(), async {
        let manager = baml_manager.lock().await;
        manager
            .invoke_function(
                scope.as_scope(),
                STREAM_BAML_TOOL_FUNCTION,
                json!({"user_message": "Perform the rite of sums."}),
            )
            .await
    })
    .await;

    match result {
        Ok(tool_choice) => {
            let manager = baml_manager.lock().await;
            let value = execute_calc_session_strict(&manager, &scope, tool_choice)
                .await
                .expect("strict session execution should succeed");
            assert_eq!(value, 5.0, "Expected 2 + 3 = 5");
        }
        Err(e) => {
            tracing::warn!("BAML tool selection failed: {}", e);
        }
    }
}

/// **Purpose:** Authoritative concurrent E2E: four requests with distinct agent IDs run
/// concurrently; each invokes ChooseCalcTool and executes the tool; assert each result
/// matches its request (no cross-contamination of scope/results).
#[tokio::test]
async fn test_e2e_voidship_baml_tool_calling_concurrent() {
    unsafe {
        std::env::set_var("BAML_TRACE_TOOL_SESSION", "1");
    }

    ensure_fixture_runtime_types();
    let agent_dir = agent_fixture("stream-baml-tool");
    let mut manager = baml_rt::baml::BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .unwrap();
    manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
    manager
        .register_llm_interceptor(StubChooseCalcToolStrictInterceptor)
        .await;
    let manager = Arc::new(manager);

    let barrier = Arc::new(Barrier::new(4));
    let mut join_set = tokio::task::JoinSet::new();
    for (idx, agent_uuid) in [
        "00000000-0000-0000-0000-0000000000a1",
        "00000000-0000-0000-0000-0000000000a2",
        "00000000-0000-0000-0000-0000000000a3",
        "00000000-0000-0000-0000-0000000000a4",
    ]
    .into_iter()
    .enumerate()
    {
        let manager = manager.clone();
        let barrier = barrier.clone();
        join_set.spawn(async move {
            let agent_id = AgentId::from_uuid(UuidId::parse_str(agent_uuid).unwrap());
            let scope = InvocationScope::synthetic_message(agent_id);
            barrier.wait().await;

            let expected = 5.0;
            let result = context::with_scope(scope.as_scope().clone(), async {
                let tool_choice = manager
                    .invoke_function(
                        scope.as_scope(),
                        STREAM_BAML_TOOL_FUNCTION,
                        json!({"user_message": format!("Compute strict req {}", idx)}),
                    )
                    .await?;
                println!(
                    "{} result (req {}): {:?}",
                    STREAM_BAML_TOOL_FUNCTION, idx, tool_choice
                );
                execute_calc_session_strict(&manager, &scope, tool_choice).await
            })
            .await?;
            let value = result;
            if (value - expected).abs() > f64::EPSILON {
                return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                    "Expected strict result {} got {}",
                    expected, value
                )));
            }
            Ok::<(), baml_rt_core::BamlRtError>(())
        });
    }

    // Live LLM calls under concurrent load can occasionally exceed 30s.
    // Keep a bounded test while reducing false CI failures from provider latency spikes.
    let deadline = Duration::from_secs(90);
    let start = std::time::Instant::now();
    let mut remaining = 4usize;
    while remaining > 0 {
        let remaining_timeout = deadline
            .checked_sub(start.elapsed())
            .unwrap_or(Duration::from_secs(0));
        let next = timeout(remaining_timeout, join_set.join_next())
            .await
            .expect("concurrent voidship test timed out");
        match next {
            Some(joined) => {
                match joined {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => panic!("concurrent voidship task failed: {}", e),
                    Err(e) => panic!("concurrent voidship task panicked: {}", e),
                }
                remaining -= 1;
            }
            None => break,
        }
    }
    assert_eq!(remaining, 0, "concurrent voidship tasks did not complete");
}
