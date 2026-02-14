#![allow(clippy::print_stdout)]
//! End-to-end tests verifying the BAML → QuickJS → LLM pipeline completes.
//!
//! These tests assert on **runtime contracts** (invocation succeeds, returns a
//! string, streaming yields chunks) — never on the content of LLM responses,
//! which is non-deterministic.

use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, UuidId};
use serde_json::json;
use test_support::common::{require_api_key, setup_baml_runtime_default, setup_bridge};
use uuid::Uuid;

#[tokio::test]
async fn test_e2e_simple_greeting_with_llm() {
    let api_key = require_api_key();
    tracing::info!("Using OpenRouter API key (length: {})", api_key.len());

    let baml_manager = setup_baml_runtime_default();
    let mut bridge = setup_bridge(baml_manager).await;

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);

    let result = context::with_scope(scope.as_scope().clone(), async {
        bridge
            .invoke_function(&scope, "SimpleGreeting", json!({ "name": "E2E Test User" }))
            .await
    })
    .await;

    let value = result.expect("BAML invocation should succeed");
    assert!(
        value.is_string(),
        "BAML SimpleGreeting should return a string"
    );
}

#[tokio::test]
async fn test_e2e_streaming_greeting() {
    let _ = require_api_key();

    let baml_manager = setup_baml_runtime_default();
    let mut bridge = setup_bridge(baml_manager).await;

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

    let result = context::with_scope(scope.as_scope().clone(), async {
        bridge.evaluate(Some(&scope), js_code).await
    })
    .await;

    let parsed = result.expect("Streaming invocation should succeed");
    let chunks = parsed
        .get("chunks")
        .and_then(|c| c.as_array())
        .expect("Streaming result should contain a chunks array");
    assert!(!chunks.is_empty(), "Stream should yield at least one chunk");
}
