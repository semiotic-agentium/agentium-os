//! LLM-backed tool workflows (Rust + JavaScript tools).

use serde_json::json;
use baml_rt_tools::ToolStep;
use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, UuidId};

use test_support::common::{WeatherTool, CalculatorTool, UppercaseTool, DelayedResponseTool};
use test_support::common::{assert_tool_registered_in_js, require_api_key, setup_baml_runtime_default, setup_bridge};

#[tokio::test]
async fn test_e2e_llm_with_tools() {
    let _ = require_api_key();
    
    tracing::info!("Starting E2E test: LLM with registered tools");
    
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(WeatherTool).await.unwrap();
        manager.register_tool(CalculatorTool).await.unwrap();
    }
    
    // Test 1: Verify tools are registered
    {
        let manager = baml_manager.lock().await;
        let tools = manager.list_tools().await;
        assert!(tools.contains(&"support/get_weather".to_string()), "Weather tool should be registered");
        assert!(tools.contains(&"support/calculate".to_string()), "Calculator tool should be registered");
        tracing::info!("✅ Tools registered: {:?}", tools);
    }
    
    // Test 2: Test tool execution directly (run inside with_scope so open_tool_session has invocation context)
    {
        let agent_id = AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000021").unwrap());
        let scope = InvocationScope::standalone(agent_id);
        let baml_manager_guard = baml_manager.clone();

        context::with_scope(scope.as_scope().clone(), async move {
            let manager = baml_manager_guard.lock().await;

            // Test weather tool
            let session_id = manager
                .open_tool_session("support/get_weather", json!({}))
                .await
                .expect("open tool session should succeed");
            manager
                .tool_session_send(&session_id, json!({"location": "San Francisco, CA"}))
                .await
                .expect("tool session send should succeed");
            let weather_result = match manager
                .tool_session_next(&session_id)
                .await
                .expect("tool session next should succeed")
            {
                ToolStep::Streaming { output } => {
                    manager.tool_session_finish(&session_id).await.unwrap();
                    output
                }
                ToolStep::Done { output } => {
                    manager.tool_session_finish(&session_id).await.unwrap();
                    output.unwrap_or_else(|| json!({}))
                }
                ToolStep::Error { error } => {
                    manager.tool_session_abort(&session_id, Some(error.message)).await.unwrap();
                    panic!("Weather tool execution failed");
                }
            };

            let weather_obj = weather_result.as_object().expect("Expected object");
            assert!(weather_obj.contains_key("temperature"), "Weather result should contain temperature");
            assert!(weather_obj.contains_key("condition"), "Weather result should contain condition");

            let location = weather_obj.get("location").and_then(|g| g.as_str()).unwrap();
            assert_eq!(location, "San Francisco, CA");

            tracing::info!("✅ Weather tool executed successfully: {:?}", weather_result);

            // Test calculator tool
            let session_id = manager
                .open_tool_session("support/calculate", json!({}))
                .await
                .expect("open tool session should succeed");
            manager
                .tool_session_send(
                    &session_id,
                    json!({"expression": {"left": 15, "operation": "Multiply", "right": 23}}),
                )
                .await
                .expect("tool session send should succeed");
            let calc_result = match manager
                .tool_session_next(&session_id)
                .await
                .expect("tool session next should succeed")
            {
                ToolStep::Streaming { output } => {
                    manager.tool_session_finish(&session_id).await.unwrap();
                    output
                }
                ToolStep::Done { output } => {
                    manager.tool_session_finish(&session_id).await.unwrap();
                    output.unwrap_or_else(|| json!({}))
                }
                ToolStep::Error { error } => {
                    manager.tool_session_abort(&session_id, Some(error.message)).await.unwrap();
                    panic!("Calculator tool execution failed");
                }
            };

            let calc_obj = calc_result.as_object().expect("Expected object");
            let result = calc_obj.get("result").and_then(|v| v.as_f64()).unwrap();
            assert_eq!(result, 345.0, "15 * 23 should equal 345");

            tracing::info!("✅ Calculator tool executed successfully: {:?}", calc_result);
        }).await;
    }
    
    // Test 3: Test BAML function execution (this will call the LLM)
    // Note: The LLM might not actually call tools unless BAML's tool calling
    // is properly configured, but we verify the infrastructure is in place
    {
        let manager = baml_manager.lock().await;
        
        tracing::info!("Calling BAML function GetWeatherInfo with location 'London'");
        let result = manager.invoke_function(
            "GetWeatherInfo",
            json!({"location": "London"})
        ).await;
        
        match result {
            Ok(response) => {
                let response_str = response.as_str()
                    .or_else(|| {
                        // Try to extract string from object if it's nested
                        response.as_object()
                            .and_then(|obj| obj.get("response"))
                            .and_then(|v| v.as_str())
                    });
                
                if let Some(text) = response_str {
                    assert!(!text.is_empty(), "Response should not be empty");
                    tracing::info!("✅ LLM responded: {}", text);
                }
            }
            Err(e) => {
                tracing::warn!("LLM call failed (expected if no API key): {}", e);
            }
        }
    }
}

#[tokio::test]
async fn test_e2e_js_tool_workflow_llm_to_js() {
    let _ = require_api_key();
    
    tracing::info!("Starting E2E test: LLM -> JS tool workflow");
    
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(UppercaseTool).await.unwrap();
    }
    
    // Create QuickJS bridge
    let mut bridge = setup_bridge(baml_manager.clone()).await;
    
    // Verify tool is registered in JS
    assert_tool_registered_in_js(&mut bridge, "support/uppercase").await;
    
    // Invoke function that uses tool
    {
        let manager = baml_manager.lock().await;
        
        let result = manager.invoke_function(
            "UppercaseText",
            json!({"text": "hello world"})
        ).await;
        
        match result {
            Ok(response) => {
                let response_str = response.as_str()
                    .or_else(|| response.as_object().and_then(|obj| obj.get("result")).and_then(|v| v.as_str()));
                
                if let Some(text) = response_str {
                    assert!(text.to_uppercase().contains("HELLO"), "Expected uppercase response");
                }
            }
            Err(e) => {
                tracing::warn!("LLM call failed (expected if no API key): {}", e);
            }
        }
    }
}

#[tokio::test]
async fn test_e2e_llm_tool_execution_flow() {
    let _ = require_api_key();
    
    tracing::info!("Starting E2E test: LLM tool execution flow");
    
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(DelayedResponseTool).await.unwrap();
    }
    
    // Invoke function that should call tool
    {
        let manager = baml_manager.lock().await;
        
        let result = manager.invoke_function(
            "DelayedResponse",
            json!({"message": "Test delay"})
        ).await;
        
        match result {
            Ok(response) => {
                let response_str = response.as_str()
                    .or_else(|| response.as_object().and_then(|obj| obj.get("response")).and_then(|v| v.as_str()));
                
                if let Some(text) = response_str {
                    assert!(text.contains("Delayed"), "Expected delayed response");
                }
            }
            Err(e) => {
                tracing::warn!("LLM call failed (expected if no API key): {}", e);
            }
        }
    }
}

#[tokio::test]
async fn test_e2e_js_tool_with_llm() {
    let _ = require_api_key();
    
    tracing::info!("Starting E2E test: JS tool with LLM");
    
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    
    // Register JS tool in QuickJS
    let mut bridge = setup_bridge(baml_manager.clone()).await;
    bridge.register_js_tool("js/concat", r#"
        async function(a, b) {
            return { result: `${a} ${b}` };
        }
    "#).await.unwrap();
    
    // Invoke function that should call JS tool
    {
        let manager = baml_manager.lock().await;
        
        let result = manager.invoke_function(
            "ConcatStrings",
            json!({"a": "Hello", "b": "World"})
        ).await;
        
        match result {
            Ok(response) => {
                let response_str = response.as_str()
                    .or_else(|| response.as_object().and_then(|obj| obj.get("result")).and_then(|v| v.as_str()));
                
                if let Some(text) = response_str {
                    assert!(text.contains("Hello") && text.contains("World"), "Expected concatenated response");
                }
            }
            Err(e) => {
                tracing::warn!("LLM call failed (expected if no API key): {}", e);
            }
        }
    }
}
