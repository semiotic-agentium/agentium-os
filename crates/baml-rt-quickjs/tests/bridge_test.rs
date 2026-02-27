//! Tests for QuickJS bridge integration

#![recursion_limit = "256"]

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt::{baml::BamlRuntimeManager, quickjs_bridge::QuickJSBridge};
use baml_rt_core::{
    bus::{BusWithEffects, EffectEmitter, EffectLiveness},
    context::{self, InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_tools::{BamlTool, bundles::BundleType};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::Mutex, task::LocalSet};
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
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.lock().await;
        m.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000013").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id).await.unwrap();
    bridge.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
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
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager
        .register_tool(ScopeEchoTool)
        .await
        .expect("register tool");
    let manager = Arc::new(Mutex::new(manager));
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.lock().await;
        m.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000014").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id).await.unwrap();
    bridge.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");

    // Single-chunk "stream" via tool session (same as non-stream test): no BAML stream function required.
    bridge
        .register_js_tool(
            "js/scope_stream",
            r#"async function(args) {
                const session = await openToolSession("test/scope_echo");
                await session.send(args || {});
                const step = await session.continue();
                const result = step && step.output ? step.output : {};
                return [result];
            }"#,
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

                    let context_id_for_js = context_id.clone();
                    let message_id_for_js = message_id.clone();
                    let task_id_for_js = task_id.clone();
                    let result = context::with_scope(scope, async move {
                        let mut bridge = bridge.lock().await;
                        bridge
                            .invoke_js_tool(
                                &invocation_scope,
                                "js/scope_stream",
                                json!({
                                    "context_id": context_id_for_js.as_str(),
                                    "message_id": message_id_for_js.as_str(),
                                    "task_id": task_id_for_js.as_str(),
                                }),
                            )
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

/// Regression test for error-path cleanup in `invoke_js_function_stream`.
///
/// If a stream invocation fails (e.g. missing JS function), the permit, session map
/// entry, LIFO context, and JS global overrides must all be cleaned up. Otherwise the
/// next stream invocation would deadlock on the semaphore or see stale globals.
///
/// Sequence:
/// 1. Force failure: `invoke_js_function_stream("nonExistentStreamFn", ...)` → Err
/// 2. Immediately start a valid stream via `invoke_js_function_stream` + `collect_into_channel_owned`
/// 3. Assert the second stream succeeds (no leaked permit/session/globals)
#[tokio::test]
async fn test_failed_stream_does_not_leak_state() {
    let manager = BamlRuntimeManager::new().unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.lock().await;
        m.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000040").unwrap());
    let bridge = Arc::new(Mutex::new(
        QuickJSBridge::new(manager, agent_id).await.unwrap(),
    ));
    {
        let mut guard = bridge.lock().await;
        guard.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
        guard
            .register_baml_functions()
            .await
            .expect("register helpers");
    }

    // Register a valid onChatMessage that yields one chunk with a final state.
    {
        let mut guard = bridge.lock().await;
        guard
            .evaluate(
                None,
                r#"
            globalThis.onChatMessage = async function(args) {
                __chat_yield({
                    task: { status: { state: "TASK_STATE_COMPLETED" } },
                    payload: "done"
                });
            };
            "#,
            )
            .await
            .expect("register onChatMessage");
    }

    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000041").unwrap(),
    ));

    // --- Step 1: Force stream failure with a nonexistent function ---
    let failed = {
        let mut guard = bridge.lock().await;
        guard
            .invoke_js_function_stream(&scope, "nonExistentStreamFn", json!({}))
            .await
            .is_err()
    };
    assert!(
        failed,
        "invoke_js_function_stream with missing function should fail"
    );

    // --- Step 2: Start a valid stream; should NOT deadlock or fail ---
    let (session_id, yield_rx) = {
        let mut guard = bridge.lock().await;
        guard
            .invoke_js_function_stream(&scope, "onChatMessage", json!({ "message": "hello" }))
            .await
            .expect("invoke after failed stream should succeed")
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    baml_rt_quickjs::collect_into_channel_owned(
        bridge.clone(),
        session_id,
        yield_rx,
        tx,
        None,
        None,
    )
    .await
    .expect("collect after failed stream should succeed");
    let mut result_chunks = Vec::new();
    while let Some(output) = rx.recv().await {
        match output {
            baml_rt_quickjs::StreamOutput::Chunk(chunk) => {
                if chunk != Value::Null {
                    result_chunks.push(chunk);
                }
            }
            baml_rt_quickjs::StreamOutput::RelayChunk(_) => {}
            baml_rt_quickjs::StreamOutput::Terminal(_, _) => break,
        }
    }

    // --- Step 3: Verify we got the expected chunk ---
    assert!(
        !result_chunks.is_empty(),
        "second stream should have produced at least one chunk"
    );
    assert!(
        result_chunks.iter().any(|c| c
            .get("task")
            .and_then(|t| t.get("status"))
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            == Some("TASK_STATE_COMPLETED")),
        "second stream should contain the TASK_STATE_COMPLETED chunk"
    );
}

/// Unit-level: close_sessions_for_context clears all sessions for that context.
#[tokio::test]
async fn test_close_sessions_for_context_clears_sessions() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000051").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);
    let context_id = scope.as_scope().context_id().clone();

    let _session_id = context::with_scope(scope.as_scope().clone(), async {
        manager
            .lock()
            .await
            .open_tool_session(scope.as_scope(), "test/scope_echo", json!({}))
            .await
            .expect("open")
    })
    .await;

    assert_eq!(
        manager
            .lock()
            .await
            .open_session_count_for_context(&context_id)
            .await,
        1,
        "one session open before teardown"
    );

    manager
        .lock()
        .await
        .close_sessions_for_context(&context_id)
        .await
        .expect("teardown");

    assert_eq!(
        manager
            .lock()
            .await
            .open_session_count_for_context(&context_id)
            .await,
        0,
        "no sessions after close_sessions_for_context (no leak)"
    );
}

/// Teardown: when a stream invocation is finalized (collect_into_channel_owned completes),
/// tool sessions for that context must be closed so we don't leak.
#[tokio::test]
async fn test_stream_finalize_closes_tool_sessions_no_leak() {
    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();
    let manager = Arc::new(Mutex::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000050").unwrap());
    let bridge = Arc::new(Mutex::new(
        QuickJSBridge::new(manager.clone(), agent_id.clone())
            .await
            .unwrap(),
    ));
    {
        let mut guard = bridge.lock().await;
        guard
            .register_baml_functions()
            .await
            .expect("register helpers");

        // onChatMessage opens a tool session then yields a terminal chunk so collector finalizes.
        guard
            .evaluate(
                None,
                r#"
            globalThis.onChatMessage = async function(args) {
                await __tool_session_open('test/scope_echo', '{}');
                __chat_yield({
                    task: { status: { state: "TASK_STATE_COMPLETED" } },
                    payload: "done"
                });
            };
            "#,
            )
            .await
            .expect("register onChatMessage");
    }

    let scope = InvocationScope::synthetic_message(agent_id);
    let context_id = scope.as_scope().context_id().clone();

    let (session_id, yield_rx) = {
        let mut guard = bridge.lock().await;
        guard
            .invoke_js_function_stream(&scope, "onChatMessage", json!({ "message": "hi" }))
            .await
            .expect("invoke stream")
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    baml_rt_quickjs::collect_into_channel_owned(bridge, session_id, yield_rx, tx, None, None)
        .await
        .expect("collect");

    // Drain until terminal
    while let Some(output) = rx.recv().await {
        if matches!(output, baml_rt_quickjs::StreamOutput::Terminal(_, _)) {
            break;
        }
    }

    // Finalize has run; no sessions must remain for this context.
    let count = manager
        .lock()
        .await
        .open_session_count_for_context(&context_id)
        .await;
    assert_eq!(
        count, 0,
        "after stream finalize there must be no open sessions for this context (no leak)"
    );
}

#[tokio::test]
async fn test_tool_session_plan_requires_manifest_mapping() {
    tracing::info!("Test: ToolSessionPlan requires manifest mapping by source function");

    use baml_rt::baml::BamlRuntimeManager;
    use serde_json::json;

    let mut manager = BamlRuntimeManager::new().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();

    // Minimal plan with only FSM operations.
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
            .execute_tool_from_baml_result_or_value(scope.as_scope(), plan, None)
            .await
    })
    .await;

    assert!(
        result.is_err(),
        "Tool session plan without manifest mapping should fail: {:?}",
        result
    );
    let err_text = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        err_text.contains("Session plan tool could not be resolved"),
        "expected manifest mapping error, got: {}",
        err_text
    );

    tracing::info!("✅ ToolSessionPlan enforces manifest mapping with no metadata fallback");
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
    bridge.set_effect_liveness(Arc::new(BusWithEffects::new()) as Arc<dyn EffectLiveness>);
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
                !msg.trim().is_empty(),
                "invoke_function error should not be empty: {}",
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
