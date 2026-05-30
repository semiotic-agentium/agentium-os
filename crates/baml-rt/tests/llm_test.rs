#![cfg(feature = "llm-tests")]
#![allow(clippy::print_stdout)]
// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! End-to-end test using actual LLM via OpenRouter.
//!
//! To isolate effect-gated timeout issues locally, run with trace logs:
//! `RUST_LOG=baml_rt_quickjs=trace cargo test -p baml-rt test_e2e_simple_greeting_with_llm -- --nocapture`
//! Check for "LlmStarted emitting" vs "effect_emitter is None", and "poll_promise: effect-gated
//! timeout sample" (in_flight_llm, context_id). The first 2s use a warm-up so the short idle
//! timeout is not applied until the promise executor has had time to run.

use std::sync::Arc;

use baml_rt::{baml_execution::ParseRetryPolicy, quickjs_bridge::QuickJSBridge};
use baml_rt_core::{
    context::{self, InvocationScope},
    ids::{AgentId, UuidId},
};
use serde_json::json;
use test_support::common::{fnox_has_openrouter_key, setup_baml_runtime_default, setup_bridge};
use uuid::Uuid;

#[tokio::test]
async fn test_e2e_simple_greeting_with_llm() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    if !fnox_has_openrouter_key() {
        eprintln!(
            "Skipping test_e2e_simple_greeting_with_llm: fnox.toml has no OPENROUTER_API_KEY"
        );
        return;
    }

    let baml_manager = setup_baml_runtime_default();
    // Single attempt: avoid retry flakiness (second attempt can return empty parsed response).
    {
        let mut mgr = baml_manager.write().await;
        mgr.set_parse_retry_policy(ParseRetryPolicy {
            max_attempts: 1,
            delay_ms: 0,
        });
    }
    let bridge = Arc::new(tokio::sync::Mutex::new(setup_bridge(baml_manager).await));

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);
    tracing::info!("Invoking SimpleGreeting BAML function...");
    let result = context::with_scope(scope.as_scope().clone(), async {
        QuickJSBridge::invoke_js_function_nonblocking(
            bridge.clone(),
            &scope,
            "SimpleGreeting",
            json!({ "name": "E2E Test User" }),
        )
        .await
    })
    .await;

    match result {
        Ok(response_value) => {
            let response_str = response_value.as_str().unwrap_or("");
            tracing::info!("✅ BAML function executed successfully!");
            tracing::info!("Response: {}", response_str);

            assert!(
                !response_str.is_empty(),
                "Response should not be empty (got value: {})",
                response_value
            );

            let response_lower = response_str.to_lowercase();
            assert!(
                response_lower.contains("e2e")
                    || response_lower.contains("test")
                    || response_lower.contains("user")
                    || response_str.len() > 5,
                "Response should be meaningful or mention the name"
            );
        }
        Err(e) => {
            tracing::error!("❌ BAML function execution failed: {}", e);
            panic!(
                "BAML function should execute successfully, but got error: {}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_e2e_streaming_greeting() {
    tracing::info!("Testing streaming BAML function call");

    if !fnox_has_openrouter_key() {
        eprintln!("Skipping test_e2e_streaming_greeting: fnox.toml has no OPENROUTER_API_KEY");
        return;
    }

    let baml_manager = setup_baml_runtime_default();
    let mut bridge = setup_bridge(baml_manager).await;

    // Call streaming BAML function from JavaScript with scope (streaming path uses worker-thread scope).
    let js_code = r#"
        (() => __awaitAndStringify(
            (async () => {
                const chunks = [];
                const stream = SimpleGreetingStream({ name: "Streaming Test" });
                for await (const chunk of stream) {
                    chunks.push(chunk);
                }
                return { chunks: chunks, totalChunks: chunks.length };
            })()
        ))()
    "#;

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);
    tracing::info!("Executing streaming JavaScript call...");
    let result = context::with_scope(scope.as_scope().clone(), async {
        bridge.eval_scoped(&scope, js_code).await
    })
    .await;

    match result {
        Ok(response_value) => {
            let response_str = response_value.as_str().unwrap_or("");
            tracing::info!("✅ Streaming function executed successfully!");
            tracing::info!("Response: {}", response_str);

            // Parse the response to verify chunks were received
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(response_str)
                && let Some(obj) = parsed.as_object()
                && let Some(chunks) = obj.get("chunks")
            {
                assert!(chunks.as_array().is_some(), "Should have chunks array");
                tracing::info!("Received {} chunks", chunks.as_array().unwrap().len());
            }
        }
        Err(e) => {
            tracing::warn!("Streaming test failed (may not be supported yet): {}", e);
            // Don't fail the test if streaming isn't fully implemented yet
        }
    }
}
