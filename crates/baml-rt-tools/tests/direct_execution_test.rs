//! Integration tests for direct BAML tool execution (Rust + JS tools).

use baml_rt::A2aAgent;
use baml_rt_core::context::InvocationScope;
use serde_json::json;
use std::sync::Arc;
use test_support::support;

#[tokio::test]
async fn test_direct_tool_execution_rust_and_js() {
    let agent = A2aAgent::builder()
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .build()
        .await
        .expect("agent build");

    {
        let runtime = agent.runtime();
        let mut runtime = runtime.lock().await;
        runtime
            .register_tool(support::tools::CalculatorTool)
            .await
            .expect("register rust tool");
    }

    let scope = InvocationScope::standalone(agent.agent_id().clone());
    let runtime = agent.runtime();

    let rust_result = {
        let mgr = runtime.lock().await;
        mgr.execute_tool_with_scope(
            scope.as_scope(),
            "support/calculate",
            json!({"expression": {"left": 6, "operation": "Multiply", "right": 7}}),
        )
        .await
        .expect("execute rust tool")
    };

    assert_eq!(
        rust_result.get("result").and_then(|v| v.as_f64()),
        Some(42.0)
    );

    agent
        .register_js_tool(
            "js/add",
            "Adds two numbers",
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "number"},
                    "b": {"type": "number"}
                },
                "required": ["a", "b"]
            }),
            r#"(args) => ({ sum: args.a + args.b })"#,
        )
        .await
        .expect("register js tool");

    let js_result = {
        let mgr = runtime.lock().await;
        mgr.execute_tool_with_scope(scope.as_scope(), "js/add", json!({"a": 10, "b": 5}))
            .await
            .expect("execute js tool")
    };

    assert_eq!(js_result.get("sum").and_then(|v| v.as_i64()), Some(15));
}
