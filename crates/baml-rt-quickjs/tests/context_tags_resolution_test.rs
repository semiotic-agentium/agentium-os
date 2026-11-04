// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conversation context tags are resolved once, for the BAML function that actually
//! runs, and only when that function's `InvocationContextRequirements` allow it.
//!
//! Guards two regressions:
//! 1. `start_stream_session` projected history eagerly for the JavaScript entrypoint
//!    (`onChatMessage`), before any agent code ran.
//! 2. `BamlExecutor::execute_function` treated "no tags supplied" as "build them
//!    yourself", so a manifest opt-out (`conversation_history: false`) projected anyway.

#![recursion_limit = "256"]

use std::sync::{Arc, Mutex};

use baml_rt::{
    baml::BamlRuntimeManager,
    baml_execution::ConversationContextProvider,
    interceptor::{InterceptorDecision, LLMCallContext, LLMInterceptor},
};
use baml_rt_core::{
    InvocationContextRequirements, context,
    ids::{AgentId, UuidId},
};
use serde_json::{Value, json};
use tokio::sync::RwLock;

/// Records the function name each projection was requested for. Returns no rows, so
/// assertions never depend on projection formatting.
#[derive(Default)]
struct RecordingProvider {
    calls: Mutex<Vec<String>>,
}

impl RecordingProvider {
    fn recorded(&self) -> Vec<String> {
        self.calls.lock().expect("recording provider lock").clone()
    }
}

#[async_trait::async_trait]
impl ConversationContextProvider for RecordingProvider {
    async fn conversation_history_json(
        &self,
        _scope: &context::RuntimeScope,
        function_name: &str,
    ) -> baml_rt_core::Result<Option<Value>> {
        self.calls
            .lock()
            .expect("recording provider lock")
            .push(function_name.to_string());
        Ok(None)
    }
}

struct StubChooseCalcToolStreamInterceptor;

#[async_trait::async_trait]
impl LLMInterceptor for StubChooseCalcToolStreamInterceptor {
    async fn intercept_llm_call(
        &self,
        context: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        if context.function_id.prompt_name().as_str() == "ChooseCalcToolStream" {
            Ok(InterceptorDecision::Substitute(Value::String("5".into())))
        } else {
            Ok(InterceptorDecision::Allow)
        }
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &baml_rt_core::Result<serde_json::Value>,
        _duration_ms: u64,
    ) {
    }
}

/// Manager loaded from the `stream-baml-tool` fixture (so an executor exists) with a
/// recording provider and the stubbed LLM call.
async fn fixture_manager(provider: Arc<RecordingProvider>) -> Option<BamlRuntimeManager> {
    test_support::common::ensure_fixture_runtime_types();
    let agent_dir = test_support::common::agent_fixture("stream-baml-tool");
    if !agent_dir.join("baml_src").exists() {
        eprintln!("Skipping test: stream-baml-tool fixture not found");
        return None;
    }

    let mut manager = BamlRuntimeManager::builder().build().unwrap();
    manager.load_schema(agent_dir.to_str().unwrap()).unwrap();
    manager
        .register_llm_interceptor(StubChooseCalcToolStreamInterceptor)
        .await;
    manager.set_conversation_context_provider(provider);
    Some(manager)
}

fn synthetic_scope(seed: u8) -> context::InvocationScope {
    let agent_id = AgentId::from_uuid(
        UuidId::parse_str(&format!("00000000-0000-0000-0000-0000000000{seed:02x}")).unwrap(),
    );
    context::InvocationScope::synthetic_message(agent_id)
}

/// A plain BAML call projects for its own function name.
#[tokio::test]
async fn baml_function_projects_for_its_own_name() {
    let provider = Arc::new(RecordingProvider::default());
    let Some(manager) = fixture_manager(provider.clone()).await else {
        return;
    };

    let scope = synthetic_scope(0xc4);
    let out = manager
        .invoke_function(
            &scope,
            "ChooseCalcToolStream",
            json!({ "user_message": "add 2 and 3" }),
        )
        .await;
    assert!(out.is_ok(), "stubbed call should succeed: {out:?}");

    assert_eq!(
        provider.recorded(),
        vec!["ChooseCalcToolStream".to_string()],
        "exactly one projection, for the invoked function"
    );
}

/// A function that opted out of conversation history is never projected, even though
/// the executor is the one creating the context manager.
#[tokio::test]
async fn opted_out_function_is_never_projected() {
    let provider = Arc::new(RecordingProvider::default());
    let Some(mut manager) = fixture_manager(provider.clone()).await else {
        return;
    };

    let mut requirements = std::collections::BTreeMap::new();
    requirements.insert(
        "ChooseCalcToolStream".to_string(),
        InvocationContextRequirements {
            conversation_history: false,
            tool_schema_prelude: false,
        },
    );
    manager.set_function_context_requirements(requirements);

    let scope = synthetic_scope(0xc5);
    let out = manager
        .invoke_function(
            &scope,
            "ChooseCalcToolStream",
            json!({ "user_message": "add 2 and 3" }),
        )
        .await;
    assert!(out.is_ok(), "stubbed call should succeed: {out:?}");

    assert!(
        provider.recorded().is_empty(),
        "opted-out function must not project; saw {:?}",
        provider.recorded()
    );
}

/// Starting a JavaScript stream for the entrypoint must not project anything: the
/// entrypoint is not a BAML function and has no context requirements.
#[tokio::test]
async fn entrypoint_stream_start_does_not_project() {
    let provider = Arc::new(RecordingProvider::default());
    let Some(manager) = fixture_manager(provider.clone()).await else {
        return;
    };
    let manager = Arc::new(RwLock::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000c6").unwrap());
    let bridge = Arc::new(tokio::sync::Mutex::new(
        baml_rt::quickjs_bridge::QuickJSBridge::new(manager, agent_id)
            .await
            .unwrap(),
    ));

    {
        let mut guard = bridge.lock().await;
        guard
            .register_baml_functions()
            .await
            .expect("register helpers");
        guard
            .eval_sync(
                r#"
                globalThis.onChatMessage = async function(_args) {
                    __chat_yield({ task: { status: { state: "TASK_STATE_COMPLETED" } }, payload: "done" });
                };
                "#,
            )
            .await
            .expect("register onChatMessage");
    }

    let scope = synthetic_scope(0xc7);
    let started = {
        let mut guard = bridge.lock().await;
        guard
            .invoke_js_function_stream(&scope, "onChatMessage", json!({ "message": "hi" }))
            .await
    };
    assert!(started.is_ok(), "stream start should succeed: {started:?}");

    assert!(
        provider.recorded().is_empty(),
        "stream start must not project for the JS entrypoint; saw {:?}",
        provider.recorded()
    );
}
