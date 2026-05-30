// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![recursion_limit = "256"]

mod common;

use std::sync::Arc;

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::ContextId;
use baml_rt_provenance::SurrealProvenanceStore;
use serde_json::Value;
use test_support::{common::send_stream_request, support::a2a::A2aInMemoryClient};
use tokio::time::Duration;

async fn collect_responses(
    agent: &A2aAgent,
    request: serde_json::Value,
) -> baml_rt::Result<Vec<serde_json::Value>> {
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await?;
    let chunks = baml_rt_core::collect_a2a_stream_one_shot(stream).await;
    Ok(chunks
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect())
}

/// Minimal agent that yields one chunk and signals completion so the host's collect()
/// returns immediately (no 60s safety timeout). Uses TASK_STATE_COMPLETED so chunk_has_final_state is true.
async fn setup_agent(store: Arc<SurrealProvenanceStore>) -> A2aAgent {
    let js_code = r#"
        globalThis.onChatMessage = async function(message) {
            __chat_yield({
                task: {
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_COMPLETED" }
                }
            });
            __chat_yield({
                task: {
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_COMPLETED" }
                }
            });
        };
    "#;
    common::provenance::build_provenance_agent(store, js_code).await
}

fn expect_context_id(responses: Vec<Value>) -> String {
    let response = responses.into_iter().next().expect("response");
    let result = response.get("result").cloned().expect("missing result");
    let content = result.get("chunk").cloned().unwrap_or(result);
    let task = content
        .get("task")
        .and_then(Value::as_object)
        .expect("task");
    task.get("contextId")
        .and_then(Value::as_str)
        .expect("contextId")
        .to_string()
}

#[tokio::test]
async fn test_context_id_propagates_across_agents() {
    let writer1 = common::provenance::build_surreal_test_store().await;
    let writer2 = common::provenance::build_surreal_test_store().await;
    let agent1 = setup_agent(writer1).await;
    let agent2 = setup_agent(writer2.clone()).await;

    let request = send_stream_request("msg-1", "hello", "corr-2-1", None);
    let responses = collect_responses(&agent1, request)
        .await
        .expect("a2a handle");
    let context_id = expect_context_id(responses);

    let client = A2aInMemoryClient::new_for_chat_parity(Arc::new(agent2));
    let request = send_stream_request(
        "msg-2",
        "forward",
        "corr-2-2",
        Some(ContextId::parse_temporal(&context_id).expect("context id")),
    );
    let responses = client.send(request).await.expect("agent2 handle");
    let propagated = expect_context_id(responses);
    assert_eq!(
        propagated, context_id,
        "expected forwarded request to preserve context id across agents"
    );
}

/// Context is explicit per request: transport builds scope from the request's context_id
/// and runs the handler under that scope. We do not rely on task locals; this test
/// verifies that each response carries the context_id from its request.
#[tokio::test]
async fn test_context_id_preserved_per_request() {
    let writer = common::provenance::build_surreal_test_store().await;
    let agent = setup_agent(writer).await;

    let context_ids: Vec<ContextId> = (0..4).map(|i| ContextId::new(10, i as u64)).collect();
    let request_timeout = if std::env::var_os("CI").is_some() {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(10)
    };
    for (idx, context_id) in context_ids.iter().enumerate() {
        let request = send_stream_request(
            &format!("msg-{idx}"),
            "hello",
            &format!("corr-2-{}", idx + 3),
            Some(context_id.clone()),
        );
        let responses = tokio::time::timeout(request_timeout, collect_responses(&agent, request))
            .await
            .expect("request timeout (agent must yield TASK_STATE_COMPLETED so collect returns)")
            .expect("a2a handle");
        let got = expect_context_id(responses);
        assert_eq!(
            got,
            context_id.as_str().to_string(),
            "expected response context_id to match request context_id"
        );
    }
}
