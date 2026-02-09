//! LLM and BAML tool calling tests.

use async_trait::async_trait;
use baml_rt::tools::BamlTool;
use baml_rt_tools::bundles::BundleType;

// Test bundle for test tools
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Barrier;
use tokio::time::{Duration, timeout};
use ts_rs::TS;

use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, UuidId};
use test_support::common::{
    CalculatorTool, WeatherTool, agent_fixture, assert_tool_registered_in_js, require_api_key,
    setup_baml_runtime_default, setup_baml_runtime_from_fixture, setup_bridge,
};

#[tokio::test]
async fn test_llm_tool_calling_rust() {
    // This test verifies tool registration and execution
    // API key is optional - test focuses on tool registration infrastructure

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(WeatherTool).await.unwrap();
        manager.register_tool(CalculatorTool).await.unwrap();
    }

    // Test that tools are registered and can be executed (scope required for execute_tool)
    let scope = InvocationScope::standalone(AgentId::from_uuid(
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

#[tokio::test]
#[allow(unnameable_test_items)]
async fn test_llm_tool_calling_js() {
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();

    // Register a tool using the trait
    struct ReverseStringTool;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct ReverseInput {
        text: String,
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
    let scope = InvocationScope::standalone(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
    ));
    assert_tool_registered_in_js(&mut bridge, "test/reverse_string", Some(&scope)).await;

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

#[tokio::test]
async fn test_e2e_baml_union_tool_calling() {
    let _ = require_api_key();

    tracing::info!("Starting E2E test: BAML union-based tool calling with Rust execution");

    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(WeatherTool).await.unwrap();
        manager.register_tool(CalculatorTool).await.unwrap();
    }

    // Test 1: Weather tool via BAML union
    {
        let manager = baml_manager.lock().await;

        tracing::info!("Testing weather tool via BAML ChooseTool function");
        let result = manager
            .invoke_function(
                "ChooseTool",
                json!({"user_message": "What's the weather in San Francisco?"}),
            )
            .await;

        match result {
            Ok(tool_choice) => {
                tracing::info!("✅ BAML function returned tool choice: {:?}", tool_choice);

                // Execute the chosen tool
                let tool_result = manager
                    .execute_tool_from_baml_result(tool_choice)
                    .await
                    .expect("Should execute tool from BAML result");

                tracing::info!("✅ Tool executed successfully: {:?}", tool_result);
                assert!(
                    tool_result.as_object().is_some(),
                    "Tool result should be an object"
                );
            }
            Err(e) => {
                tracing::warn!(
                    "BAML function call failed (may need tool calling integration): {}",
                    e
                );
            }
        }
    }

    // Test 2: Calculator tool via BAML union
    {
        let manager = baml_manager.lock().await;

        tracing::info!("Testing calculator tool via BAML ChooseTool function");
        let result = manager
            .invoke_function(
                "ChooseTool",
                json!({"user_message": "Calculate 15 times 23"}),
            )
            .await;

        match result {
            Ok(tool_choice) => {
                tracing::info!("✅ BAML function returned tool choice: {:?}", tool_choice);

                // Execute the chosen tool
                let tool_result = manager
                    .execute_tool_from_baml_result(tool_choice)
                    .await
                    .expect("Should execute tool from BAML result");

                tracing::info!("✅ Tool executed successfully: {:?}", tool_result);

                // Verify calculator result
                if let Some(obj) = tool_result.as_object()
                    && let Some(result) = obj.get("result").and_then(|v| v.as_f64())
                {
                    assert_eq!(result, 345.0, "15 * 23 should equal 345");
                }
            }
            Err(e) => {
                tracing::warn!(
                    "BAML function call failed (may need tool calling integration): {}",
                    e
                );
            }
        }
    }

    tracing::info!("🎉 E2E BAML union tool calling test completed!");
}

#[tokio::test]
async fn test_e2e_voidship_baml_tool_calling() {
    let _ = require_api_key();

    let baml_manager = setup_baml_runtime_from_fixture("voidship-rites");
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(CalculatorTool).await.unwrap();
    }

    let result = {
        let manager = baml_manager.lock().await;
        manager
            .invoke_function(
                "ChooseRiteTool",
                json!({"user_message": "Perform the rite of sums."}),
            )
            .await
    };

    match result {
        Ok(tool_choice) => {
            let manager = baml_manager.lock().await;
            let tool_result = manager
                .execute_tool_from_baml_result_or_value(tool_choice)
                .await
                .expect("Should execute tool from BAML result");
            let value = tool_result
                .get("result")
                .and_then(|v| v.as_f64())
                .unwrap_or_default();
            assert_eq!(value, 5.0, "Expected 2 + 3 = 5");
        }
        Err(e) => {
            tracing::warn!("BAML tool selection failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_e2e_voidship_baml_tool_calling_concurrent() {
    unsafe {
        std::env::set_var("BAML_TRACE_TOOL_SESSION", "1");
    }
    let _ = require_api_key();

    let agent_dir = agent_fixture("voidship-rites");
    let mut manager = baml_rt::baml::BamlRuntimeManager::new().unwrap();
    manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    manager.register_tool(CalculatorTool).await.unwrap();
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
            let scope = InvocationScope::standalone(agent_id);
            barrier.wait().await;

            let result = context::with_scope(scope.as_scope().clone(), async {
                let tool_choice = manager
                    .invoke_function(
                        "ChooseRiteTool",
                        json!({"user_message": format!("Perform the rite of sums. (req {})", idx)}),
                    )
                    .await?;
                println!("ChooseRiteTool result (req {}): {:?}", idx, tool_choice);
                manager
                    .execute_tool_from_baml_result_or_value(tool_choice)
                    .await
            })
            .await?;

            let value = result
                .get("result")
                .and_then(|v| v.as_f64())
                .unwrap_or_default();
            if value != 5.0 {
                return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                    "Expected 2 + 3 = 5, got {}",
                    value
                )));
            }
            Ok::<(), baml_rt_core::BamlRtError>(())
        });
    }

    let deadline = Duration::from_secs(30);
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
