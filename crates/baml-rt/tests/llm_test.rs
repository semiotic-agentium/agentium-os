#![allow(clippy::print_stdout)]
//! End-to-end tests verifying the BAML → QuickJS → LLM pipeline completes.
//!
//! These tests assert on **runtime contracts** (invocation succeeds, returns a
//! string, streaming yields chunks) — never on the content of LLM responses,
//! which is non-deterministic.

use baml_rt_core::context::{self, InvocationScope};
use baml_rt_core::ids::{AgentId, UuidId};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use test_support::common::{
    require_api_key, run_live_llm_with_retry, setup_baml_runtime_default, setup_bridge,
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[tokio::test]
async fn test_e2e_simple_greeting_with_llm() {
    let api_key = require_api_key();
    tracing::info!("Using OpenRouter API key (length: {})", api_key.len());

    let baml_manager = setup_baml_runtime_default();
    let bridge = Arc::new(Mutex::new(setup_bridge(baml_manager).await));

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);

    let value = run_live_llm_with_retry("SimpleGreeting", 3, Duration::from_secs(120), |_| {
        let bridge = Arc::clone(&bridge);
        let scope = scope.clone();
        async move {
            context::with_scope(scope.as_scope().clone(), async {
                let mut b = bridge.lock().await;
                b.invoke_function(&scope, "SimpleGreeting", json!({ "name": "E2E Test User" }))
                    .await
            })
            .await
        }
    })
    .await
    .expect("BAML invocation should succeed");
    assert!(
        value.is_string(),
        "BAML SimpleGreeting should return a string"
    );
}

#[tokio::test]
async fn test_e2e_streaming_greeting() {
    let _ = require_api_key();

    let baml_manager = setup_baml_runtime_default();
    let bridge = Arc::new(Mutex::new(setup_bridge(baml_manager).await));

    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let scope = InvocationScope::synthetic_message(agent_id);

    let chunks =
        run_live_llm_with_retry("SimpleGreetingStream", 3, Duration::from_secs(120), |_| {
            let bridge = Arc::clone(&bridge);
            let scope = scope.clone();
            async move {
                context::with_scope(scope.as_scope().clone(), async {
                    let mut b = bridge.lock().await;
                    b.invoke_function_stream(
                        &scope,
                        "SimpleGreeting",
                        json!({ "name": "Streaming Test" }),
                    )
                    .await
                })
                .await
            }
        })
        .await
        .expect("Streaming invocation should succeed");

    assert!(!chunks.is_empty(), "Stream should yield at least one chunk");
}
