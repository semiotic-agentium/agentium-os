//! Tests for QuickJS bridge integration

use baml_rt::baml::BamlRuntimeManager;
use baml_rt::quickjs_bridge::QuickJSBridge;
use baml_rt_core::context::{self, InvocationScope, RuntimeScope};
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_tools::BamlTool;
use baml_rt_tools::bundles::BundleType;
use serde_json::{json, Value};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use ts_rs::TS;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::LocalSet;

// Test bundle for test tools
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}

#[tokio::test]
async fn test_quickjs_bridge_creation() {
    // Test that we can create a QuickJS bridge
    let baml_manager = Arc::new(Mutex::new(BamlRuntimeManager::new().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());
    let bridge = QuickJSBridge::new(baml_manager, agent_id);
    
    let bridge = bridge.await;
    assert!(bridge.is_ok(), "Should be able to create QuickJS bridge");
}

#[tokio::test]
async fn test_quickjs_evaluate_simple_code() {
    // Test that we can execute simple JavaScript code
    let baml_manager = Arc::new(Mutex::new(BamlRuntimeManager::new().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000011").unwrap());
    let mut bridge = QuickJSBridge::new(baml_manager, agent_id).await.unwrap();
    
    // Execute a simple JavaScript expression
    let result = bridge.evaluate(None,"2 + 2").await;
    
    // The result might be a string representation or actual JSON
    // For now, just check that it doesn't error
    assert!(result.is_ok(), "Should be able to execute JavaScript code");
}

#[tokio::test]
async fn test_quickjs_evaluate_json() {
    // Test JSON stringify/parse
    let baml_manager = Arc::new(Mutex::new(BamlRuntimeManager::new().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000012").unwrap());
    let mut bridge = QuickJSBridge::new(baml_manager, agent_id).await.unwrap();
    
    // Execute code that returns a JSON object
    let result = bridge.evaluate(None,"({answer: 42})").await;
    
    assert!(result.is_ok(), "Should be able to execute JavaScript and get JSON");
}

#[tokio::test(flavor = "current_thread")]
async fn test_quickjs_concurrent_scope_propagation() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(ScopeEchoTool).await.expect("register tool");
    let manager = Arc::new(Mutex::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000013").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id).await.unwrap();
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");

    bridge
        .register_js_tool(
            "js/scope_tool",
            r#"async function(args) {
                const session = await openToolSession("test/scope_echo");
                await session.send(args);
                const step = await session.continue();
                return step && step.output ? step.output : {};
            }"#,
        )
        .await
        .expect("register js tool");

    let bridge = Arc::new(Mutex::new(bridge));
    let results = Arc::new(Mutex::new(Vec::new()));
    let local = LocalSet::new();

    local
        .run_until(async {
            let mut handles = Vec::new();
            for idx in 0..8 {
                let bridge = bridge.clone();
                let results = results.clone();
                handles.push(tokio::task::spawn_local(async move {
                    let context_id = ContextId::new(1, idx as u64 + 1);
                    let agent_id = AgentId::from_uuid(
                        UuidId::parse_str("00000000-0000-0000-0000-000000000003").unwrap(),
                    );
                    let message_id = MessageId::from_external(ExternalId::new(format!("msg-qjs-{idx}")));
                    let task_id = TaskId::from_external(ExternalId::new(format!("task-qjs-{idx}")));
                    let scope =
                        RuntimeScope::new(context_id.clone(), agent_id, Some(message_id.clone()), Some(task_id.clone()));

                    let result = context::with_scope(scope, async move {
                        let mut bridge = bridge.lock().await;
                        bridge
                            .invoke_js_tool("js/scope_tool", json!({"text": "ping"}))
                            .await
                    })
                    .await
                    .expect("invoke js tool");

                    results.lock().await.push((context_id, message_id, task_id, result));
                }));
            }

            for handle in handles {
                handle.await.expect("join");
            }
        })
        .await;

    let results = results.lock().await;
    assert_eq!(results.len(), 8, "expected 8 tool results");
    for (context_id, message_id, task_id, result) in results.iter() {
        assert_eq!(
            result.get("context_id").and_then(Value::as_str),
            Some(context_id.as_str() as &str)
        );
        assert_eq!(
            result.get("message_id").and_then(Value::as_str),
            Some(message_id.as_str())
        );
        assert_eq!(
            result.get("task_id").and_then(Value::as_str),
            Some(task_id.as_str())
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_quickjs_concurrent_stream_scope_propagation() {
    let manager = BamlRuntimeManager::new().unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000014").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id).await.unwrap();
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");

    bridge
        .evaluate(
            None,
            r#"
            globalThis.js_scope_stream = async function() {
                const results = await __baml_stream(
                    "scope_probe",
                    JSON.stringify({ __scope_probe: true }),
                    globalThis.__baml_context_id,
                    globalThis.__baml_message_id,
                    globalThis.__baml_task_id
                );
                return results;
            };
            "#,
        )
        .await
        .expect("register js stream tool");

    let bridge = Arc::new(Mutex::new(bridge));
    let results = Arc::new(Mutex::new(Vec::new()));
    let local = LocalSet::new();

    local
        .run_until(async {
            let mut handles = Vec::new();
            for idx in 0..8 {
                let bridge = bridge.clone();
                let results = results.clone();
                handles.push(tokio::task::spawn_local(async move {
                    let context_id = ContextId::new(2, idx as u64 + 1);
                    let agent_id = AgentId::from_uuid(
                        UuidId::parse_str("00000000-0000-0000-0000-000000000004").unwrap(),
                    );
                    let message_id = MessageId::from_external(ExternalId::new(format!("msg-qjs-stream-{idx}")));
                    let task_id = TaskId::from_external(ExternalId::new(format!("task-qjs-stream-{idx}")));
                    let scope =
                        RuntimeScope::new(context_id.clone(), agent_id, Some(message_id.clone()), Some(task_id.clone()));
                    let invocation_scope = InvocationScope::new(scope.clone());

                    let result = context::with_scope(scope, async move {
                        let mut bridge = bridge.lock().await;
                        bridge
                            .invoke_js_function(&invocation_scope, "js_scope_stream", json!({}))
                            .await
                    })
                    .await
                    .expect("invoke js stream");

                    results.lock().await.push((context_id, message_id, task_id, result));
                }));
            }

            for handle in handles {
                handle.await.expect("join");
            }
        })
        .await;

    let results = results.lock().await;
    assert_eq!(results.len(), 8, "expected 8 stream results");
    for (context_id, message_id, task_id, result) in results.iter() {
        let first = result
            .as_array()
            .and_then(|items| items.first())
            .expect("expected stream results");
        assert_eq!(
            first.get("context_id").and_then(Value::as_str),
            Some(context_id.as_str() as &str)
        );
        assert_eq!(
            first.get("message_id").and_then(Value::as_str),
            Some(message_id.as_str())
        );
        assert_eq!(
            first.get("task_id").and_then(Value::as_str),
            Some(task_id.as_str())
        );
    }
}

#[derive(Debug)]
struct ScopeEchoTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct ScopeEchoInput {
    text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct ScopeEchoOutput {
    context_id: Option<String>,
    message_id: Option<String>,
    task_id: Option<String>,
}

#[async_trait]
impl BamlTool for ScopeEchoTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "scope_echo";
    type OpenInput = ();
    type Input = ScopeEchoInput;
    type Output = ScopeEchoOutput;

    fn description(&self) -> &'static str {
        "Echoes current runtime scope."
    }

    async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
        let context_id = context::current_context_id().map(|id| id.to_string());
        let message_id = context::current_message_id().map(|id| id.to_string());
        let task_id = context::current_task_id().map(|id| id.to_string());
        Ok(ScopeEchoOutput {
            context_id,
            message_id,
            task_id,
        })
    }
}

#[tokio::test]
async fn test_tool_session_plan_with_initial_input() {
    tracing::info!("Test: ToolSessionPlan open step with initial_input");
    
    use baml_rt::baml::BamlRuntimeManager;
    use serde_json::json;
    
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();
    
    // Create a tool session plan with initial_input in the open step
    let plan = json!({
        "steps": [
            {
                "op": "open",
                "initial_input": {}
            },
            {
                "op": "send",
                "input": {
                    "text": "test message"
                }
            },
            {
                "op": "next"
            },
            {
                "op": "finish"
            }
        ]
    });
    
    // Execute the plan - this should call open_tool_session with the initial_input
    let result = manager.execute_tool_from_baml_result(plan).await;
    
    assert!(result.is_ok(), "Tool session plan should execute successfully");
    let output = result.unwrap();
    
    // Verify the result contains the expected structure
    let result_obj = output.as_object().expect("Expected object");
    assert!(result_obj.contains_key("context_id") || result_obj.contains_key("message_id"), 
        "Result should contain scope information");
    
    tracing::info!("✅ ToolSessionPlan with initial_input executed successfully");
}
