//! Comprehensive end-to-end tests for the trait-based tool system
//!
//! Tests cover:
//! - Tool registration using the BamlTool trait
//! - Tool execution from Rust
//! - Tool execution from JavaScript via QuickJS
//! - Tool metadata and listing
//! - **E2E: Actual LLM calls that invoke registered tools**


use baml_rt::tools::BamlTool;
use baml_rt_core::context;
use baml_rt_core::ids::{AgentId, UuidId};
use serde_json::json;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use ts_rs::TS;

use test_support::common::{
    WeatherTool,
    CalculatorTool,
    assert_tool_registered_in_js,
    require_api_key,
    setup_baml_runtime_default,
    setup_bridge,
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

/// Test tool for arithmetic operations
struct ArithmeticTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct ArithmeticInput {
    operation: ArithmeticOp,
    a: f64,
    b: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct ArithmeticOutput {
    operation: ArithmeticOp,
    a: f64,
    b: f64,
    result: f64,
    formatted: String,
}

#[async_trait]
impl BamlTool for ArithmeticTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "arithmetic";
    type OpenInput = ();
    type Input = ArithmeticInput;
    type Output = ArithmeticOutput;
    
    fn description(&self) -> &'static str {
        "Performs basic arithmetic operations: add, subtract, multiply, divide"
    }
    
    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        let op_str = match args.operation {
            ArithmeticOp::Add => "+",
            ArithmeticOp::Subtract => "-",
            ArithmeticOp::Multiply => "*",
            ArithmeticOp::Divide => "/",
        };
        let result = match args.operation {
            ArithmeticOp::Add => args.a + args.b,
            ArithmeticOp::Subtract => args.a - args.b,
            ArithmeticOp::Multiply => args.a * args.b,
            ArithmeticOp::Divide => if args.b != 0.0 { args.a / args.b } else { 0.0 },
        };

        Ok(ArithmeticOutput {
            operation: args.operation,
            a: args.a,
            b: args.b,
            result,
            formatted: format!("{} {} {} = {}", args.a, op_str, args.b, result),
        })
    }
}

/// Test tool for string manipulation
struct StringManipulationTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
enum StringOp {
    Uppercase,
    Lowercase,
    Reverse,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct StringManipulationInput {
    operation: StringOp,
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct StringManipulationOutput {
    operation: StringOp,
    original: String,
    result: String,
}

#[async_trait]
impl BamlTool for StringManipulationTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "string_manipulation";
    type OpenInput = ();
    type Input = StringManipulationInput;
    type Output = StringManipulationOutput;
    
    fn description(&self) -> &'static str {
        "Performs string manipulation operations: uppercase, lowercase, reverse"
    }
    
    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        let result = match args.operation {
            StringOp::Uppercase => args.text.to_uppercase(),
            StringOp::Lowercase => args.text.to_lowercase(),
            StringOp::Reverse => args.text.chars().rev().collect(),
        };

        Ok(StringManipulationOutput {
            operation: args.operation,
            original: args.text,
            result,
        })
    }
}

#[tokio::test]
async fn test_e2e_trait_tool_registration_rust_execution() {
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    
    // Register tools using trait system
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(ArithmeticTool).await.unwrap();
        manager.register_tool(StringManipulationTool).await.unwrap();
    }
    
    // Execute tools from Rust
    {
        let manager = baml_manager.lock().await;
        
        let arithmetic_result = manager.execute_tool(
            "test/arithmetic",
            json!({"operation": "multiply", "a": 7, "b": 6})
        ).await.unwrap();
        
        let result = arithmetic_result.get("result").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(result, 42.0, "7 * 6 should equal 42");
        
        let string_result = manager.execute_tool(
            "test/string_manipulation",
            json!({"operation": "reverse", "text": "baml"})
        ).await.unwrap();
        
        let result = string_result.get("result").and_then(|v| v.as_str()).unwrap();
        assert_eq!(result, "lmab", "Reversing 'baml' should give 'lmab'");
    }
}

#[tokio::test]
async fn test_e2e_trait_tool_js_registration() {
    // Set up BAML runtime and bridge
    let baml_manager = setup_baml_runtime_default();
    
    // Register tools using trait system
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(ArithmeticTool).await.unwrap();
    }
    
    let mut bridge = setup_bridge(baml_manager.clone()).await;
    
    // Verify tool is registered in JS
    assert_tool_registered_in_js(&mut bridge, "test/arithmetic").await;
}

#[tokio::test]
async fn test_e2e_trait_tool_metadata_and_listing() {
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    
    // Register tools
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(ArithmeticTool).await.unwrap();
        manager.register_tool(StringManipulationTool).await.unwrap();
    }
    
    // Test listing and metadata
    {
        let manager = baml_manager.lock().await;
        let tools = manager.list_tools().await;
        
        assert!(tools.contains(&"test/arithmetic".to_string()));
        assert!(tools.contains(&"test/string_manipulation".to_string()));
        
        let arithmetic_meta = manager.get_tool_metadata("test/arithmetic").await.unwrap();
        assert_eq!(arithmetic_meta.name.to_string(), "test/arithmetic");
        assert!(arithmetic_meta.description.contains("arithmetic"));
    }
}

#[tokio::test]
async fn test_e2e_trait_tool_llm_calling() {
    let _ = require_api_key();
    
    // Set up BAML runtime
    let baml_manager = setup_baml_runtime_default();
    
    // Register tools
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(WeatherTool).await.unwrap();
        manager.register_tool(CalculatorTool).await.unwrap();
    }
    
    // Invoke a function that may call tools
    {
        let manager = baml_manager.lock().await;
        
        let agent_id = AgentId::from_uuid(
            UuidId::parse_str("00000000-0000-0000-0000-000000000010")
                .expect("valid test uuid"),
        );
        let result = context::with_agent_id(agent_id, async {
            manager
                .invoke_function("ChooseTool", json!({"user_message": "What is 42 times 7?"}))
                .await
        })
        .await;

        match result {
            Ok(_) => {}
            Err(err) => panic!("invoke_function failed: {err}"),
        }
    }
}
