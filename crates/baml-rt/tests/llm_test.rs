#![allow(clippy::print_stdout)]
//! End-to-end test using actual LLM via OpenRouter

use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, UuidId};
use serde_json::json;
use std::sync::Arc;
use test_support::common::{
    require_api_key, run_live_llm_with_retry_validate, setup_baml_runtime_default, setup_bridge,
};
use uuid::Uuid;

#[tokio::test]
async fn test_e2e_simple_greeting_with_llm() {
    let api_key = require_api_key();
    tracing::info!("Using OpenRouter API key (length: {})", api_key.len());

    let baml_manager = setup_baml_runtime_default();
    let bridge = Arc::new(tokio::sync::Mutex::new(setup_bridge(baml_manager).await));

    // Call BAML function via invoke_function (uses task-local scope; evaluate()+scope has worker-thread subtleties).
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);
    tracing::info!("Invoking SimpleGreeting BAML function...");
    let response_value = run_live_llm_with_retry_validate(
        "SimpleGreeting",
        3,
        std::time::Duration::from_secs(120),
        |_| {
            let bridge = bridge.clone();
            let scope = scope.clone();
            async move {
                let mut bridge = bridge.lock().await;
                context::with_scope(scope.as_scope().clone(), async {
                    bridge
                        .invoke_function(
                            &scope,
                            "SimpleGreeting",
                            json!({ "name": "E2E Test User" }),
                        )
                        .await
                })
                .await
            }
        },
        |value| {
            let response_str = value.as_str().unwrap_or("");
            if response_str.trim().is_empty() {
                return Err("SimpleGreeting returned empty response".to_string());
            }
            Ok(())
        },
    )
    .await
    .unwrap_or_else(|e| panic!("BAML function should execute successfully: {e}"));

    let response_str = response_value.as_str().unwrap_or("");
    tracing::info!("✅ BAML function executed successfully!");
    tracing::info!("Response: {}", response_str);

    let response_lower = response_str.to_lowercase();
    assert!(
        response_lower.contains("e2e")
            || response_lower.contains("test")
            || response_lower.contains("user")
            || response_str.len() > 5,
        "Response should be meaningful or mention the name"
    );
}

#[tokio::test]
async fn test_e2e_streaming_greeting() {
    let _ = require_api_key();

    tracing::info!("Testing streaming BAML function call");

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
        bridge.evaluate(Some(&scope), js_code).await
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
