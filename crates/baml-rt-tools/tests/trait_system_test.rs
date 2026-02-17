//! Trait-based tool system: registration, Rust execution, JS visibility, metadata.
//!
//! **Purpose:** Verify that tools implemented with the BamlTool trait can be
//! registered, executed from Rust (with scope), exposed to QuickJS (openToolSession),
//! and that list_tools / get_tool_metadata return correct data. No LLM calls;
//! E2E authority for LLM→tool is in baml-rt (voidship) and runner.

use async_trait::async_trait;
use baml_rt::tools::BamlTool;
use baml_rt_core::context;
use baml_rt_core::ids::{AgentId, UuidId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use baml_rt_tools::bundles::BundleType;
use test_support::common::{
    assert_tool_registered_in_js, setup_baml_runtime_default, setup_bridge,
};

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
            ArithmeticOp::Divide => {
                if args.b != 0.0 {
                    args.a / args.b
                } else {
                    0.0
                }
            }
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

/// **Purpose:** Register ArithmeticTool and StringManipulationTool, then execute
/// them from Rust (execute_tool) inside an invocation scope and assert correct results.
#[tokio::test]
async fn test_e2e_trait_tool_registration_rust_execution() {
    let baml_manager = setup_baml_runtime_default();

    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(ArithmeticTool).await.unwrap();
        manager.register_tool(StringManipulationTool).await.unwrap();
    }

    let scope = context::InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap(),
    ));

    context::with_scope(scope.as_scope().clone(), async {
        let manager = baml_manager.lock().await;

        let arithmetic_result = manager
            .execute_tool(
                scope.as_scope(),
                "test/arithmetic",
                json!({"operation": "multiply", "a": 7, "b": 6}),
            )
            .await
            .unwrap();

        let result = arithmetic_result
            .get("result")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert_eq!(result, 42.0, "7 * 6 should equal 42");

        let string_result = manager
            .execute_tool(
                scope.as_scope(),
                "test/string_manipulation",
                json!({"operation": "reverse", "text": "baml"}),
            )
            .await
            .unwrap();

        let result = string_result
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(result, "lmab", "Reversing 'baml' should give 'lmab'");
    })
    .await;
}

/// **Purpose:** Register ArithmeticTool, create QuickJS bridge; assert the tool
/// is visible to JS via assert_tool_registered_in_js (scope required for Rust tools).
#[tokio::test]
async fn test_e2e_trait_tool_js_registration() {
    let baml_manager = setup_baml_runtime_default();

    // Register tools using trait system
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(ArithmeticTool).await.unwrap();
    }

    let mut bridge = setup_bridge(baml_manager.clone()).await;

    let scope = context::InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-00000000000a").unwrap(),
    ));
    // Verify tool is registered in JS (scope required for Rust tools via openToolSession)
    assert_tool_registered_in_js(&mut bridge, "test/arithmetic", &scope).await;
}

/// **Purpose:** After registering two tools, list_tools() contains their names
/// and get_tool_metadata("test/arithmetic") returns name and description.
#[tokio::test]
async fn test_e2e_trait_tool_metadata_and_listing() {
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

// E2E trait-tool LLM calling retired: authority is baml-rt voidship + concurrent tests.
