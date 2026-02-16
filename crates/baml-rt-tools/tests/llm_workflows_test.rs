//! Tool registration and listing integration tests (no LLM).
//!
//! **Purpose:** Verify that tools registered via the trait appear in the runtime
//! tool list. Full LLM→tool E2E lives in `baml-rt` (voidship) and `baml-agent-runner`
//! (streaming); tool lifecycle and scope attribution are covered by property tests.

use test_support::common::{setup_baml_runtime_default, CalculatorTool, WeatherTool};

/// **Purpose:** After registering WeatherTool and CalculatorTool, list_tools() must
/// contain `support/get_weather` and `support/calculate`. No LLM or bridge.
#[tokio::test]
async fn test_tool_registration_and_listing() {
    let baml_manager = setup_baml_runtime_default();
    {
        let mut manager = baml_manager.lock().await;
        manager.register_tool(WeatherTool).await.unwrap();
        manager.register_tool(CalculatorTool).await.unwrap();
    }
    let manager = baml_manager.lock().await;
    let tools = manager.list_tools().await;
    assert!(tools.contains(&"support/get_weather".to_string()));
    assert!(tools.contains(&"support/calculate".to_string()));
}
