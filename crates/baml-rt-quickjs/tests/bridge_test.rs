//! Tests for QuickJS bridge integration

#![recursion_limit = "256"]

use async_trait::async_trait;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::quickjs_bridge::QuickJSBridge;
use baml_rt_core::context::{self, InvocationScope, RuntimeScope};
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_tools::BamlTool;
use baml_rt_tools::bundles::BundleType;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::LocalSet;
use ts_rs::TS;

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

/// Property-style: for each of several expressions, evaluate returns Ok.
#[tokio::test]
async fn test_quickjs_evaluate_expressions() {
    let baml_manager = Arc::new(Mutex::new(BamlRuntimeManager::new().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000011").unwrap());
    let mut bridge = QuickJSBridge::new(baml_manager, agent_id).await.unwrap();

    let expressions = ["2 + 2", "({answer: 42})", "null", "1"];
    for (i, code) in expressions.iter().enumerate() {
        let result = bridge.evaluate(None, code).await;
        assert!(
            result.is_ok(),
            "expression[{}] {:?} should evaluate: {:?}",
            i,
            code,
            result.err()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_quickjs_concurrent_scope_propagation() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager
        .register_tool(ScopeEchoTool)
        .await
        .expect("register tool");
    let manager = Arc::new(Mutex::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000013").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id).await.unwrap();
    bridge.set_effect_liveness(Arc::new(baml_rt_core::bus::BusWithEffects::new()));
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
                    let message_id =
                        MessageId::from_external(ExternalId::new(format!("msg-qjs-{idx}")));
                    let task_id = TaskId::from_external(ExternalId::new(format!("task-qjs-{idx}")));
                    let scope = RuntimeScope::task_scope(
                        context_id.clone(),
                        agent_id,
                        message_id.clone(),
                        task_id.clone(),
                    );
                    let invocation_scope = InvocationScope::new(scope.clone());
                    let context_id_for_js = context_id.clone();
                    let message_id_for_js = message_id.clone();
                    let task_id_for_js = task_id.clone();

                    let result = context::with_scope(scope, async move {
                        let mut bridge = bridge.lock().await;
                        bridge
                            .invoke_js_tool(
                                &invocation_scope,
                                "js/scope_tool",
                                json!({
                                    "text": "ping",
                                    "context_id": context_id_for_js.as_str(),
                                    "message_id": message_id_for_js.as_str(),
                                    "task_id": task_id_for_js.as_str(),
                                }),
                            )
                            .await
                    })
                    .await
                    .expect("invoke js tool");

                    results
                        .lock()
                        .await
                        .push((context_id, message_id, task_id, result));
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
    bridge.set_effect_liveness(Arc::new(baml_rt_core::bus::BusWithEffects::new()));
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");

    bridge
        .evaluate(
            None,
            r#"
            globalThis.js_scope_stream = async function(args) {
                const results = await __baml_stream(
                    "scope_probe",
                    JSON.stringify({ __scope_probe: true })
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
                    let message_id =
                        MessageId::from_external(ExternalId::new(format!("msg-qjs-stream-{idx}")));
                    let task_id =
                        TaskId::from_external(ExternalId::new(format!("task-qjs-stream-{idx}")));
                    let scope = RuntimeScope::task_scope(
                        context_id.clone(),
                        agent_id,
                        message_id.clone(),
                        task_id.clone(),
                    );
                    let invocation_scope = InvocationScope::new(scope.clone());

                    let result = context::with_scope(scope, async move {
                        let mut bridge = bridge.lock().await;
                        bridge
                            .invoke_js_function(&invocation_scope, "js_scope_stream", json!({}))
                            .await
                    })
                    .await
                    .expect("invoke js stream");

                    results
                        .lock()
                        .await
                        .push((context_id, message_id, task_id, result));
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
    context_id: Option<String>,
    message_id: Option<String>,
    task_id: Option<String>,
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

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(ScopeEchoOutput {
            context_id: args.context_id,
            message_id: args.message_id,
            task_id: args.task_id,
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

    // Minimal plan: open with initial_input (for tool name resolution), then finish.
    let plan = json!({
        "steps": [
            { "op": "open", "initial_input": {} },
            { "op": "finish" }
        ]
    });

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);

    let result = context::with_scope(scope.as_scope().clone(), async {
        manager
            .execute_tool_from_baml_result_or_value(scope.as_scope(), plan)
            .await
    })
    .await;

    assert!(
        result.is_ok(),
        "Tool session plan should execute successfully: {:?}",
        result.as_ref().err()
    );
    let _output = result.unwrap();
    // Plan executed under scope; output shape depends on tool/session (may be object or null).

    tracing::info!("✅ ToolSessionPlan with initial_input executed successfully");
}

/// Operations requiring invocation scope must be called with explicit scope.
/// invoke_function requires explicit scope; missing function should produce a function-level error.
#[tokio::test]
async fn test_invoke_function_with_explicit_scope_fails_for_missing_function() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();

    // invoke_function without scope (requires bridge)
    let baml_manager = Arc::new(Mutex::new(BamlRuntimeManager::new().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000030").unwrap());
    let mut bridge = QuickJSBridge::new(baml_manager, agent_id).await.unwrap();
    bridge.set_effect_liveness(Arc::new(baml_rt_core::bus::BusWithEffects::new()));
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");
    let invoke_scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000031").unwrap(),
    ));
    let result = bridge
        .invoke_function(&invoke_scope, "SomeFunction", json!({}))
        .await;
    match result {
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("SomeFunction") || msg.contains("function"),
                "invoke_function error should mention function resolution: {}",
                msg
            );
        }
        Ok(val) => {
            let error_msg = if let Some(obj) = val.as_object() {
                obj.get("error")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else if let Some(s) = val.as_str() {
                serde_json::from_str::<Value>(s).ok().and_then(|parsed| {
                    parsed
                        .get("error")
                        .and_then(|v| v.as_str())
                        .map(|msg| msg.to_string())
                })
            } else {
                None
            };

            assert!(
                error_msg.is_some(),
                "invoke_function should report error for missing function, got: {}",
                val
            );
        }
    }
}
