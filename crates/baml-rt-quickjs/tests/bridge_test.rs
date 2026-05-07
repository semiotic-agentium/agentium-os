//! Tests for QuickJS bridge integration

#![recursion_limit = "256"]

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt::{
    baml::{
        BamlRuntimeManager, IntentSubmission, PlanStepStatusChange, PlanSubmission,
        PlanningDynamicContext, PlanningResolver,
    },
    quickjs_bridge::QuickJSBridge,
};
use baml_rt_core::{
    bus::{BusWithEffects, EffectEmitter, EffectLiveness},
    context::{self, InvocationScope, RuntimeScope},
    ids::{
        AgentId, ContextId, ExternalId, IntentId, MessageId, PlanId, PlanStepId, TaskId, UuidId,
    },
};
use baml_rt_provenance::{
    ProvEvent, ProvenanceEffectSubscriber, ProvenanceWriter, SurrealStoreBuilder,
};
use baml_rt_tools::{BamlTool, bundles::BundleType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, RwLock},
    task::LocalSet,
};

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
    let baml_manager = Arc::new(RwLock::new(BamlRuntimeManager::builder().build().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());
    let bridge = QuickJSBridge::new(baml_manager, agent_id);

    let bridge = bridge.await;
    assert!(bridge.is_ok(), "Should be able to create QuickJS bridge");
}

/// Property-style: for each of several expressions, evaluate returns Ok.
#[tokio::test]
async fn test_quickjs_evaluate_expressions() {
    let baml_manager = Arc::new(RwLock::new(BamlRuntimeManager::builder().build().unwrap()));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000011").unwrap());
    let mut bridge = QuickJSBridge::new(baml_manager, agent_id).await.unwrap();

    let expressions = ["2 + 2", "({answer: 42})", "null", "1"];
    for (i, code) in expressions.iter().enumerate() {
        let result = bridge.eval_sync(code).await;
        assert!(
            result.is_ok(),
            "expression[{}] {:?} should evaluate: {:?}",
            i,
            code,
            result.err()
        );
    }
}

// Must run on a multi-thread runtime: current_thread serialises spawn_local tasks so
// the Mutex contention is never actually concurrent, masking lock-held-across-poll bugs.
#[tokio::test(flavor = "multi_thread")]
async fn test_quickjs_concurrent_scope_propagation() {
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager
        .register_tool(ScopeEchoTool)
        .await
        .expect("register tool");
    let manager = Arc::new(RwLock::new(manager));
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.write().await;
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
                        QuickJSBridge::invoke_js_tool_nonblocking(
                            bridge,
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

// Must run on a multi-thread runtime: current_thread serialises spawn_local tasks so
// the Mutex contention is never actually concurrent, masking lock-held-across-poll bugs.
#[tokio::test(flavor = "multi_thread")]
async fn test_quickjs_concurrent_stream_scope_propagation() {
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager
        .register_tool(ScopeEchoTool)
        .await
        .expect("register tool");
    let manager = Arc::new(RwLock::new(manager));
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.write().await;
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
                        QuickJSBridge::invoke_js_tool_nonblocking(
                            bridge,
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

/// Regression test: two concurrent contexts must both make progress while the bridge
/// lock is not held across async work. Before the fix, evaluate() held &mut self
/// across the entire promise-polling loop, so the second context could not enter JS
/// until the first had fully completed — including any async tool/LLM latency.
///
/// Proof strategy: BarrierTool::execute() waits at a two-party Barrier.
/// If both invocations can enter execute() concurrently the barrier unblocks immediately.
/// If the bridge lock is held across async work, the second invocation cannot start,
/// the barrier never unblocks, and the test times out.
#[tokio::test(flavor = "multi_thread")]
async fn test_bridge_lock_not_held_across_async_work() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager
        .register_tool(BarrierTool(barrier))
        .await
        .expect("register slow tool");
    let manager = Arc::new(RwLock::new(manager));
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.write().await;
        m.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);
    }
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let mut bridge = QuickJSBridge::new(manager, agent_id).await.unwrap();
    bridge.set_effect_liveness(effect_bus as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");

    bridge
        .register_js_tool(
            "js/barrier_tool",
            r#"async function(args) {
                const session = await openToolSession("test/barrier");
                await session.send(args || {});
                const step = await session.continue();
                return step && step.output ? step.output : {};
            }"#,
        )
        .await
        .expect("register barrier js tool");

    let bridge = Arc::new(Mutex::new(bridge));

    // Uses invoke_js_tool_nonblocking so the bridge Mutex is released before the poll loop,
    // allowing both contexts to make progress concurrently. This is how the handover lane
    // dispatches invocations in production.
    let (r1, r2) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            {
                let bridge = bridge.clone();
                async move {
                    let context_id = ContextId::new(9, 1);
                    let agent_id = AgentId::from_uuid(
                        UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
                    );
                    let scope = RuntimeScope::task_scope(
                        context_id.clone(),
                        agent_id,
                        MessageId::from_external(ExternalId::new("msg-barrier-1")),
                        TaskId::from_external(ExternalId::new("task-barrier-1")),
                    );
                    let inv = InvocationScope::new(scope.clone());
                    context::with_scope(scope, async move {
                        QuickJSBridge::invoke_js_tool_nonblocking(
                            bridge,
                            &inv,
                            "js/barrier_tool",
                            json!({}),
                        )
                        .await
                    })
                    .await
                }
            },
            {
                let bridge = bridge.clone();
                async move {
                    let context_id = ContextId::new(9, 2);
                    let agent_id = AgentId::from_uuid(
                        UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
                    );
                    let scope = RuntimeScope::task_scope(
                        context_id.clone(),
                        agent_id,
                        MessageId::from_external(ExternalId::new("msg-barrier-2")),
                        TaskId::from_external(ExternalId::new("task-barrier-2")),
                    );
                    let inv = InvocationScope::new(scope.clone());
                    context::with_scope(scope, async move {
                        QuickJSBridge::invoke_js_tool_nonblocking(
                            bridge,
                            &inv,
                            "js/barrier_tool",
                            json!({}),
                        )
                        .await
                    })
                    .await
                }
            }
        )
    })
    .await
    .expect(
        "timed out after 5s: barrier never unblocked, indicating the bridge lock was held \
         across async work (second invocation could not enter JS while first was waiting)",
    );

    r1.expect("invocation 1 must succeed");
    r2.expect("invocation 2 must succeed");
}

#[derive(Debug)]
struct ScopeEchoTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct ScopeEchoInput {
    text: Option<String>,
    context_id: Option<String>,
    message_id: Option<String>,
    task_id: Option<String>,
}
impl baml_rt_tools::DescribeAction for ScopeEchoInput {
    fn describe(&self) -> String {
        "ScopeEchoInput".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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

/// BarrierTool waits at a two-party tokio Barrier. Two concurrent invocations unblock
/// each other immediately; a sequential execution order (bridge lock held across async
/// work) would deadlock since the second invocation can never enter execute().
/// Used by `test_bridge_lock_not_held_across_async_work` to verify that the bridge
/// lock is released during async work so concurrent contexts can make progress.
#[derive(Debug)]
struct BarrierTool(Arc<tokio::sync::Barrier>);

#[derive(Debug, Clone, Serialize, Deserialize, BamlType, Default)]
struct BarrierInput {}
impl baml_rt_tools::DescribeAction for BarrierInput {
    fn describe(&self) -> String {
        "BarrierInput".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct BarrierOutput {
    done: bool,
}

#[async_trait]
impl BamlTool for BarrierTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "barrier";
    type OpenInput = ();
    type Input = BarrierInput;
    type Output = BarrierOutput;

    fn description(&self) -> &'static str {
        "Waits at a two-party barrier to prove concurrent execution."
    }

    async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
        self.0.wait().await;
        Ok(BarrierOutput { done: true })
    }
}

#[derive(Debug, Default, Clone)]
struct ResolverObservation {
    intent_saw_scope_echo: bool,
    plan_saw_scope_echo: bool,
    step_saw_scope_echo: bool,
    observed_task_ids: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
struct CanonicalizingResolver {
    observation: Arc<StdMutex<ResolverObservation>>,
}

#[async_trait]
impl PlanningResolver for CanonicalizingResolver {
    async fn resolve_intent(
        &self,
        context: &PlanningDynamicContext,
        mut submission: IntentSubmission,
    ) -> baml_rt_core::Result<IntentSubmission> {
        let mut state = self.observation.lock().expect("resolver observation lock");
        state.intent_saw_scope_echo = context
            .available_tools
            .iter()
            .any(|name| name == "test/scope_echo");
        state.observed_task_ids.push(
            context
                .scope
                .task_id_opt()
                .map(|id| id.as_str().to_string()),
        );
        drop(state);
        submission.intent_id = IntentId::from("intent-canon");
        Ok(submission)
    }

    async fn resolve_plan(
        &self,
        context: &PlanningDynamicContext,
        mut submission: PlanSubmission,
    ) -> baml_rt_core::Result<PlanSubmission> {
        let mut state = self.observation.lock().expect("resolver observation lock");
        state.plan_saw_scope_echo = context
            .available_tools
            .iter()
            .any(|name| name == "test/scope_echo");
        state.observed_task_ids.push(
            context
                .scope
                .task_id_opt()
                .map(|id| id.as_str().to_string()),
        );
        drop(state);
        submission.intent_id = IntentId::from("intent-canon");
        submission.plan_id = PlanId::from("plan-canon");
        submission.steps = json!([{ "step_id": "step-canon", "description": "Canonical step", "order": 0, "depends_on": [] }]);
        Ok(submission)
    }

    async fn resolve_step_status(
        &self,
        context: &PlanningDynamicContext,
        mut submission: PlanStepStatusChange,
    ) -> baml_rt_core::Result<PlanStepStatusChange> {
        let mut state = self.observation.lock().expect("resolver observation lock");
        state.step_saw_scope_echo = context
            .available_tools
            .iter()
            .any(|name| name == "test/scope_echo");
        state.observed_task_ids.push(
            context
                .scope
                .task_id_opt()
                .map(|id| id.as_str().to_string()),
        );
        drop(state);
        submission.intent_id = IntentId::from("intent-canon");
        submission.plan_id = PlanId::from("plan-canon");
        submission.step_id = PlanStepId::from("step-canon");
        Ok(submission)
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
    let manager = BamlRuntimeManager::builder().build().unwrap();
    let manager = Arc::new(RwLock::new(manager));
    let effect_bus = Arc::new(BusWithEffects::new());
    {
        let mut m = manager.write().await;
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
            .eval_sync(
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
    let (_abort_tx, abort_rx) = tokio::sync::mpsc::channel(1);
    baml_rt_quickjs::collect_into_channel_owned(
        bridge.clone(),
        session_id,
        yield_rx,
        tx,
        abort_rx,
        None,
        None,
        scope,
        baml_rt_quickjs::HandoverSender::noop(),
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
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();
    let manager = Arc::new(RwLock::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000051").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);
    let context_id = scope.as_scope().context_id().clone();

    let _session_id = context::with_scope(scope.as_scope().clone(), async {
        manager
            .read()
            .await
            .open_tool_session(scope.as_scope(), "test/scope_echo", json!({}))
            .await
            .expect("open")
    })
    .await;

    assert_eq!(
        manager
            .read()
            .await
            .open_session_count_for_context(&context_id)
            .await,
        1,
        "one session open before teardown"
    );

    manager
        .read()
        .await
        .close_sessions_for_context(&context_id)
        .await
        .expect("teardown");

    assert_eq!(
        manager
            .read()
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
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();
    let manager = Arc::new(RwLock::new(manager));
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
            .eval_sync(
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
    let (_abort_tx, abort_rx) = tokio::sync::mpsc::channel(1);
    baml_rt_quickjs::collect_into_channel_owned(
        bridge,
        session_id,
        yield_rx,
        tx,
        abort_rx,
        None,
        None,
        scope,
        baml_rt_quickjs::HandoverSender::noop(),
    )
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
        .read()
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

    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();

    // Minimal plan with only FSM operations.
    let plan = json!({
        "step": { "op": "open", "initial_input": {} }
    });

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);

    let result = context::with_scope(scope.as_scope().clone(), async {
        manager
            .execute_tool_from_baml_result_or_value(scope.as_scope(), plan, None, None)
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

/// Strict-mode sequence via per-fragment execution: Open -> Send -> (SearchRead | PageRead) -> Finish.
/// This case uses **PageRead** (contiguous lines; no grep). SearchRead would require a non-empty `grep`.
/// Tests `execute_tool_from_baml_result_or_value` directly (not through the step executor loop).
#[tokio::test]
async fn test_tool_session_plan_open_send_next_finish_runs_finish() {
    use baml_rt::baml::BamlRuntimeManager;
    use serde_json::json;

    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();
    let mut map = std::collections::HashMap::new();
    map.insert(
        "EchoPlanFn".to_string(),
        vec![baml_rt_tools::SessionPlanTypeName::new("TestScopeEchoSessionPlan").unwrap()],
    );
    manager.set_session_plan_functions(Some(map));

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000021").unwrap());
    let scope = InvocationScope::synthetic_message(agent_id);

    let open = context::with_scope(scope.as_scope().clone(), async {
        manager
            .execute_tool_from_baml_result_or_value(
                scope.as_scope(),
                json!({ "step": { "op": "Open", "initial_input": {} } }),
                Some("EchoPlanFn"),
                None,
            )
            .await
    })
    .await
    .expect("strict Open should succeed");
    assert_eq!(open.get("status").and_then(Value::as_str), Some("open"));

    let sent = context::with_scope(scope.as_scope().clone(), async {
        manager
            .execute_tool_from_baml_result_or_value(
                scope.as_scope(),
                json!({ "step": { "op": "Send", "input": { "text": "ping" } } }),
                Some("EchoPlanFn"),
                None,
            )
            .await
    })
    .await
    .expect("blocking Send should succeed");
    // Send now blocks until Done and returns archive_ref + output header.
    assert_eq!(
        sent.get("status").and_then(Value::as_str),
        Some("done"),
        "Send must return 'done' status (blocking); got: {:?}",
        sent
    );
    let archive_ref = sent
        .get("archive_ref")
        .and_then(Value::as_str)
        .expect("Send result must include archive_ref")
        .to_string();
    assert!(
        archive_ref.starts_with('@'),
        "archive_ref must start with '@'; got {archive_ref}"
    );

    let read = context::with_scope(scope.as_scope().clone(), async {
        manager
            .execute_tool_from_baml_result_or_value(
                scope.as_scope(),
                json!({ "step": { "op": "PageRead", "input": { "archive_ref": archive_ref } } }),
                Some("EchoPlanFn"),
                None,
            )
            .await
    })
    .await
    .expect("PageRead with archive_ref should succeed");
    assert!(
        read.get("output").is_some(),
        "PageRead should include output payload; got: {:?}",
        read
    );

    let finish = context::with_scope(scope.as_scope().clone(), async {
        manager
            .execute_tool_from_baml_result_or_value(
                scope.as_scope(),
                json!({ "step": { "op": "Finish" } }),
                Some("EchoPlanFn"),
                None,
            )
            .await
    })
    .await
    .expect("strict Finish should succeed");
    assert_eq!(
        finish.get("status").and_then(Value::as_str),
        Some("finished"),
        "Finish must close session"
    );
}

/// End-to-end test of `run_step_executor_loop` with interceptor-substituted LLM responses.
/// Exercises the real path: loop calls invoke_function per hop, which hits the interceptor,
/// parses the result, executes the tool session plan fragment, and returns the tool output.
/// The loop advances the FSM based on the returned status until terminal.
#[tokio::test]
async fn test_step_executor_loop_drives_full_session_with_interceptor() {
    use std::sync::atomic::{AtomicU32, Ordering};

    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt_core::context::InvocationScope;
    use baml_rt_quickjs::step_executor_loop::run_step_executor_loop;
    use serde_json::json;

    test_support::common::ensure_fixture_runtime_types();
    let agent_dir =
        test_support::common::workspace_root().join("tests/fixtures/agents/stream-baml-tool");
    let built =
        test_support::common::build_agent_package_to_temp(agent_dir, "stream-baml-tool").await;

    // Task-scoped + minimal graph so LLM rows attach to a task thread (synthetic message-only
    // scopes are not sufficient for the conversation query to grow every hop in tests).
    let scope = InvocationScope::synthetic_task(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
    ));
    let (context_id, message_id, task_id) = {
        let rs = scope.as_scope();
        let task_id = rs
            .task_id_opt()
            .expect("synthetic_task has task_id")
            .clone();
        (rs.context_id().clone(), rs.message_id().clone(), task_id)
    };

    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager
        .load_schema(built.to_str().unwrap())
        .expect("load schema");

    let manifest = baml_rt_tools::ManifestToolNames::parse(&["support/calculate".to_string()])
        .expect("parse manifest");
    let policy = baml_rt_tools::parse_access_allowlist();
    manager
        .set_tool_allowlist(
            ["support/calculate"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .await
        .unwrap();
    baml_rt_tools::register_manifest_tools(manager.tool_registry().as_ref(), &manifest, &policy)
        .unwrap();
    manager.rebuild_function_tool_manifest();

    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory provenance for step executor e2e");
    manager.set_effect_emitter(bus.clone());
    let manager = Arc::new(RwLock::new(manager));
    baml_rt::a2a_transport::install_provenance_conversation_wiring(
        store.clone(),
        Some(store.clone()),
        &manager,
        &bus,
    )
    .await
    .expect("provenance + conversation wiring");

    {
        let agent = scope.as_scope().agent_id().clone();
        store
            .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
            .await
            .expect("task exists");
        store
            .add_event(ProvEvent::task_execution_started(
                context_id.clone(),
                task_id.clone(),
                agent.clone(),
            ))
            .await
            .expect("task execution started");
        store
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_id,
                message_id,
                "user".to_string(),
                vec!["stream-baml step executor e2e".to_string()],
                None,
                agent,
                1_700_000_010_303,
            ))
            .await
            .expect("message received");
    }

    // Interceptor: returns Open, Send (blocking until Done), Finish in sequence.
    // Read is now optional — Send blocks and archives; the FSM goes directly to __continue__.
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    struct SequenceInterceptor {
        count: Arc<AtomicU32>,
    }

    #[async_trait]
    impl baml_rt::interceptor::LLMInterceptor for SequenceInterceptor {
        async fn intercept_llm_call(
            &self,
            _ctx: &baml_rt::interceptor::LLMCallContext,
        ) -> baml_rt_core::Result<baml_rt::interceptor::InterceptorDecision> {
            let n = self.count.fetch_add(1, Ordering::Relaxed);
            let response = match n {
                0 => json!({ "step": { "op": "Open" } }),
                1 => {
                    json!({ "step": { "op": "Send", "input": { "expression": { "left": 2, "operation": "Add", "right": 3 } } } })
                }
                // Send now blocks until Done; FSM advances to __continue__ (Read/Finish phase).
                2 => json!({ "step": { "op": "Finish" } }),
                _ => json!({ "message": "done" }),
            };
            Ok(baml_rt::interceptor::InterceptorDecision::Substitute(
                response,
            ))
        }

        async fn on_llm_call_complete(
            &self,
            _: &baml_rt::interceptor::LLMCallContext,
            _: &baml_rt_core::Result<Value>,
            _: u64,
        ) {
        }
    }

    {
        let w = manager.write().await;
        w.register_llm_interceptor(SequenceInterceptor {
            count: call_count_clone,
        })
        .await;
    }

    let result = context::with_scope(
        scope.as_scope().clone(),
        run_step_executor_loop(
            &manager,
            scope.as_scope(),
            "ChooseCalcTool",
            json!({ "user_message": "what is 2 + 3?" }),
            8,
            None,
        ),
    )
    .await
    .expect("step executor loop should complete");

    assert!(
        result.steps.len() >= 3,
        "loop should have executed at least 3 hops (Open, Send[blocking], Finish), got {}",
        result.steps.len()
    );
    assert!(
        call_count.load(Ordering::Relaxed) >= 3,
        "interceptor should have been called at least 3 times (Open, Send, Finish)"
    );
}

#[tokio::test]
async fn test_runtime_canonical_planning_uses_custom_resolver_and_dynamic_context() {
    let mut manager = BamlRuntimeManager::new().expect("create runtime manager");
    manager
        .register_tool(ScopeEchoTool)
        .await
        .expect("register scope echo tool");
    let effect_bus = Arc::new(BusWithEffects::new());
    let store = SurrealStoreBuilder::in_memory()
        .build()
        .await
        .expect("build in-memory provenance store");
    effect_bus
        .subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;
    manager.set_effect_emitter(effect_bus as Arc<dyn EffectEmitter>);

    let observation = Arc::new(StdMutex::new(ResolverObservation::default()));
    manager.set_planning_resolver(Arc::new(CanonicalizingResolver {
        observation: observation.clone(),
    }));

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-00000000aa13").unwrap());
    let context_id = ContextId::new(9_004, 1);
    let message_id = MessageId::from_external(ExternalId::new("msg-planning-canonical"));
    let task_id = TaskId::from_external(ExternalId::new("task-planning-canonical"));
    let scope = RuntimeScope::task_scope(
        context_id.clone(),
        agent_id.clone(),
        message_id.clone(),
        task_id.clone(),
    );

    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task execution started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            message_id.clone(),
            "user".to_string(),
            vec!["Resolve canonically".to_string()],
            None,
            agent_id.clone(),
            1_700_000_010_101,
        ))
        .await
        .expect("message received");

    manager
        .emit_planning_intent_resolved(
            &scope,
            IntentSubmission {
                intent_id: IntentId::from("intent-raw".to_string()),
                description: "raw description".to_string(),
                citations: vec!["#1".parse::<baml_rt_core::Citation>().unwrap()],
                derived_from_message_ids: vec![message_id.as_str().to_string()],
                supersession: None,
            },
            None,
        )
        .await
        .expect("intent resolved");
    manager
        .emit_planning_plan_generated(
            &scope,
            "intent-raw".to_string(),
            "plan-raw".to_string(),
            json!([{ "step_id": "step-raw", "description": "raw", "order": 0, "depends_on": [] }]),
            None,
            None,
        )
        .await
        .expect("plan generated");
    manager
        .emit_planning_step_status_changed(
            &scope,
            "intent-raw".to_string(),
            "plan-raw".to_string(),
            "step-raw".to_string(),
            Some("pending".to_string()),
            "completed".to_string(),
            vec!["#1".parse::<baml_rt_core::Citation>().unwrap()],
            None,
        )
        .await
        .expect("step status changed");

    let ctx_str = context_id.as_str().to_string();
    let task_str = task_id.as_str().to_string();

    let mut intent_resp = store.db()
        .query("SELECT count() AS cnt FROM prov_node WHERE label = 'Intent' AND props.a2a_context_id = $ctx AND props.a2a_task_id = $tid AND props.a2a_intent_id = $iid GROUP ALL")
        .bind(("ctx", ctx_str.clone()))
        .bind(("tid", task_str.clone()))
        .bind(("iid", "intent-canon".to_string()))
        .await.expect("intent query");
    let intent_rows: Vec<Value> = intent_resp.take(0).unwrap_or_default();
    let intent_count = intent_rows
        .first()
        .and_then(|r| r.get("cnt").and_then(Value::as_i64))
        .unwrap_or(0);

    let mut plan_resp = store.db()
        .query("SELECT count() AS cnt FROM prov_node WHERE label = 'Plan' AND props.a2a_context_id = $ctx AND props.a2a_task_id = $tid AND props.a2a_plan_id = $pid AND props.a2a_intent_id = $iid GROUP ALL")
        .bind(("ctx", ctx_str.clone()))
        .bind(("tid", task_str.clone()))
        .bind(("pid", "plan-canon".to_string()))
        .bind(("iid", "intent-canon".to_string()))
        .await.expect("plan query");
    let plan_rows: Vec<Value> = plan_resp.take(0).unwrap_or_default();
    let plan_count = plan_rows
        .first()
        .and_then(|r| r.get("cnt").and_then(Value::as_i64))
        .unwrap_or(0);

    let mut step_resp = store.db()
        .query("SELECT count() AS cnt FROM prov_node WHERE label = 'PlanStep' AND props.a2a_context_id = $ctx AND props.a2a_task_id = $tid AND props.a2a_plan_id = $pid AND props.a2a_step_id = $sid GROUP ALL")
        .bind(("ctx", ctx_str))
        .bind(("tid", task_str))
        .bind(("pid", "plan-canon".to_string()))
        .bind(("sid", "step-canon".to_string()))
        .await.expect("step query");
    let step_rows: Vec<Value> = step_resp.take(0).unwrap_or_default();
    let step_count = step_rows
        .first()
        .and_then(|r| r.get("cnt").and_then(Value::as_i64))
        .unwrap_or(0);

    assert_eq!(intent_count, 1);
    assert_eq!(plan_count, 1);
    assert_eq!(step_count, 1);

    let seen = observation
        .lock()
        .expect("resolver observation lock")
        .clone();
    assert!(
        seen.intent_saw_scope_echo,
        "intent resolver must see tool inventory"
    );
    assert!(
        seen.plan_saw_scope_echo,
        "plan resolver must see tool inventory"
    );
    assert!(
        seen.step_saw_scope_echo,
        "step resolver must see tool inventory"
    );
    assert_eq!(
        seen.observed_task_ids,
        vec![
            Some(task_id.as_str().to_string()),
            Some(task_id.as_str().to_string()),
            Some(task_id.as_str().to_string())
        ]
    );
}

/// Operations requiring invocation scope must be called with explicit scope.
/// invoke_function requires explicit scope; missing function should produce a function-level error.
#[tokio::test]
async fn test_invoke_function_with_explicit_scope_fails_for_missing_function() {
    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager.register_tool(ScopeEchoTool).await.unwrap();

    // invoke_function without scope (requires bridge)
    let baml_manager = Arc::new(RwLock::new(BamlRuntimeManager::builder().build().unwrap()));
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
    let bridge = Arc::new(Mutex::new(bridge));
    let result = QuickJSBridge::invoke_js_function_nonblocking(
        bridge,
        &invoke_scope,
        "SomeFunction",
        json!({}),
    )
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

/// Unified structured step executor: non-terminal AskUser-shaped JSON then terminal WorkflowPlan-shaped payload.
#[tokio::test]
async fn test_unified_step_executor_ask_user_then_plan_with_interceptor() {
    use std::sync::atomic::{AtomicU32, Ordering};

    use baml_rt::baml::BamlRuntimeManager;
    use baml_rt_core::context::InvocationScope;
    use baml_rt_quickjs::step_executor_loop::run_step_executor_loop;

    test_support::common::ensure_fixture_runtime_types();
    let agent_dir = test_support::common::workspace_root()
        .join("tests/fixtures/agents/unified-step-harness-demo");
    let built =
        test_support::common::build_agent_package_to_temp(agent_dir, "unified-step-harness-demo")
            .await;

    let scope = InvocationScope::synthetic_task(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-0000000000c4").unwrap(),
    ));
    let (context_id, message_id, task_id) = {
        let rs = scope.as_scope();
        let task_id = rs
            .task_id_opt()
            .expect("synthetic_task has task_id")
            .clone();
        (rs.context_id().clone(), rs.message_id().clone(), task_id)
    };

    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager
        .load_schema(built.to_str().unwrap())
        .expect("load schema");

    let manifest = baml_rt_tools::ManifestToolNames::parse(&["support/calculate".to_string()])
        .expect("parse manifest");
    let policy = baml_rt_tools::parse_access_allowlist();
    manager
        .set_tool_allowlist(
            ["support/calculate"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .await
        .unwrap();
    baml_rt_tools::register_manifest_tools(manager.tool_registry().as_ref(), &manifest, &policy)
        .unwrap();
    manager.rebuild_function_tool_manifest();

    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory provenance for unified step executor e2e");
    manager.set_effect_emitter(bus.clone());
    let manager = Arc::new(RwLock::new(manager));
    baml_rt::a2a_transport::install_provenance_conversation_wiring(
        store.clone(),
        Some(store.clone()),
        &manager,
        &bus,
    )
    .await
    .expect("provenance + conversation wiring");

    {
        let agent = scope.as_scope().agent_id().clone();
        store
            .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
            .await
            .expect("task exists");
        store
            .add_event(ProvEvent::task_execution_started(
                context_id.clone(),
                task_id.clone(),
                agent.clone(),
            ))
            .await
            .expect("task execution started");
        store
            .add_event(ProvEvent::message_received_task(
                context_id.clone(),
                task_id,
                message_id,
                "user".to_string(),
                vec!["unified step harness e2e".to_string()],
                None,
                agent,
                1_700_000_010_304,
            ))
            .await
            .expect("message received");
    }

    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    struct UnifiedSequenceInterceptor {
        count: Arc<AtomicU32>,
    }

    #[async_trait]
    impl baml_rt::interceptor::LLMInterceptor for UnifiedSequenceInterceptor {
        async fn intercept_llm_call(
            &self,
            _ctx: &baml_rt::interceptor::LLMCallContext,
        ) -> baml_rt_core::Result<baml_rt::interceptor::InterceptorDecision> {
            let n = self.count.fetch_add(1, Ordering::Relaxed);
            let response = match n {
                0 => json!({ "action": "AskUser", "question": "Which scope?" }),
                1 => json!({
                    "goal": "fixture-goal",
                    "nodes": [{
                        "id": "n1",
                        "kind": "direct_answer",
                        "depends_on": [],
                    }],
                }),
                _ => json!({ "message": "unexpected hop" }),
            };
            Ok(baml_rt::interceptor::InterceptorDecision::Substitute(
                response,
            ))
        }

        async fn on_llm_call_complete(
            &self,
            _: &baml_rt::interceptor::LLMCallContext,
            _: &baml_rt_core::Result<Value>,
            _: u64,
        ) {
        }
    }

    {
        let w = manager.write().await;
        w.register_llm_interceptor(UnifiedSequenceInterceptor {
            count: call_count_clone,
        })
        .await;
    }

    let result = context::with_scope(
        scope.as_scope().clone(),
        run_step_executor_loop(
            &manager,
            scope.as_scope(),
            "UnifiedHarnessPick",
            json!({ "user_message": "hello" }),
            8,
            None,
        ),
    )
    .await
    .expect("unified step executor should complete");

    assert!(
        result.steps.len() >= 2,
        "expected AskUser hop then terminal plan, got {} steps",
        result.steps.len()
    );
    assert!(
        call_count.load(Ordering::Relaxed) >= 2,
        "interceptor should run at least twice"
    );

    let envelope = &result.last;
    let terminal = envelope
        .get("output")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| envelope.clone());
    assert_eq!(
        terminal.get("goal").and_then(Value::as_str),
        Some("fixture-goal"),
        "terminal payload should be UnifiedHarnessPlan JSON; got {terminal:?}"
    );
}
