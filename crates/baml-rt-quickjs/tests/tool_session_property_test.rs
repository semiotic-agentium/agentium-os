// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Property tests for tool session lifecycle (valid session plans).
//!
//! **Purpose:** Assert that N independent valid sessions (open → send → next → finish)
//! all complete successfully when run sequentially; validates that valid session
//! plans do not leak state and complete consistently.

#![recursion_limit = "256"]

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    context::{self, InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, UuidId},
};
use baml_rt_tools::{BamlTool, ToolStep, bundles::BundleType};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn proptest_cfg(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(cases);
    cfg.failure_persistence = None;
    cfg
}

struct Test;
impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for property tests"
    }
}

struct EchoTool;

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct EchoInput {
    text: String,
}
impl baml_rt_tools::DescribeAction for EchoInput {
    fn describe(&self) -> String {
        "EchoInput".to_string()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct EchoOutput {
    echo: String,
}

#[async_trait]
impl BamlTool for EchoTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "echo_tool";
    type OpenInput = ();
    type Input = EchoInput;
    type Output = EchoOutput;

    fn description(&self) -> &'static str {
        "Echo for session property tests"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(EchoOutput { echo: args.text })
    }
}

// Purpose: For N in 1..=4, run N sequential sessions (open, send, next, finish)
// and assert each completes without error; no session leak or cross-session state.
proptest! {
    #![proptest_config(proptest_cfg(4))]
    #[test]
    fn prop_valid_session_plans_complete(n_sessions in 1u32..=4u32) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut manager = BamlRuntimeManager::builder().build().unwrap();
            manager.register_tool(EchoTool).await.unwrap();
            let agent_id = AgentId::from_uuid(
                UuidId::parse_str("00000000-0000-0000-0000-000000000030").unwrap(),
            );
            let scope = InvocationScope::synthetic_message(agent_id);

            for _ in 0..n_sessions {
                let result = context::with_scope(scope.as_scope().clone(), async {
                    let session_id = manager
                        .open_tool_session(scope.as_scope(), "test/echo_tool", json!({}))
                        .await
                        .expect("open_tool_session");
                    manager
                        .tool_session_send(&session_id, json!({ "text": "ping" }))
                        .await
                        .expect("tool_session_send");
                    let step = manager
                        .tool_session_read(&session_id, serde_json::Value::Null)
                        .await
                        .expect("tool_session_next");
                    match step {
                        ToolStep::Streaming { output: _ } | ToolStep::Suspended { output: _ } => {
                            manager.tool_session_finish(&session_id).await.expect("finish");
                        }
                        ToolStep::Done { output: _ } => {
                            manager.tool_session_finish(&session_id).await.expect("finish");
                        }
                        ToolStep::Error { error } => {
                            let msg = error.message.clone();
                            let _ = manager.tool_session_abort(&session_id, Some(msg)).await;
                            panic!("session error: {}", error.message);
                        }
                    }
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                })
                .await;
                result.expect("session completed");
            }
        });
    }

    /// Purpose: explicit scope API must allow session open without task-local context.
    ///
    /// Property:
    /// ∀ N in [1,4], N sequential sessions opened via explicit-scope API complete.
    #[test]
    fn prop_open_tool_session_with_explicit_scope_complete(n_sessions in 1u32..=4u32) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut manager = BamlRuntimeManager::builder().build().unwrap();
            manager.register_tool(EchoTool).await.unwrap();
            let agent_id = AgentId::from_uuid(
                UuidId::parse_str("00000000-0000-0000-0000-000000000031").unwrap(),
            );
            let context_id = ContextId::new(123456, 1);
            let message_id =
                MessageId::from_external(ExternalId::new("prop-session-msg-1"));
            let explicit_scope = RuntimeScope::message_scope(context_id, agent_id, message_id);

            for _ in 0..n_sessions {
                let session_id = manager
                    .open_tool_session(
                        &explicit_scope,
                        "test/echo_tool",
                        json!({}),
                    )
                    .await
                    .expect("open_tool_session");
                manager
                    .tool_session_send(&session_id, json!({ "text": "ping" }))
                    .await
                    .expect("tool_session_send");
                let step = manager
                    .tool_session_read(&session_id, serde_json::Value::Null)
                    .await
                    .expect("tool_session_next");
                match step {
                    ToolStep::Streaming { output: _ }
                    | ToolStep::Suspended { output: _ }
                    | ToolStep::Done { output: _ } => {
                        manager.tool_session_finish(&session_id).await.expect("finish");
                    }
                    ToolStep::Error { error } => {
                        let msg = error.message.clone();
                        let _ = manager.tool_session_abort(&session_id, Some(msg)).await;
                        panic!("session error: {}", error.message);
                    }
                }
            }
        });
    }
}
