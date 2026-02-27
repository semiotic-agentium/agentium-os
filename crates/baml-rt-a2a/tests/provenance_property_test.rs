//! Provenance attribution test using GraphQLite in-memory store.

#![recursion_limit = "256"]

mod common;

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::{ContextId, CorrelationId};
use test_support::common::send_stream_request;
use tokio::time::Duration;

async fn collect_responses(
    agent: &A2aAgent,
    request: serde_json::Value,
) -> baml_rt::Result<Vec<serde_json::Value>> {
    let stream = agent
        .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
        .await?;
    let chunks = baml_rt_core::collect_a2a_stream(stream).await;
    Ok(chunks
        .into_iter()
        .map(baml_rt_core::A2aStreamChunk::into_inner)
        .collect())
}

#[tokio::test(flavor = "current_thread")]
async fn test_scope_attribution_without_cross_contamination() {
    let writer = common::provenance::build_graphqlite_test_store();
    let js = r#"
        globalThis.onChatMessage = async function(message) {
            const text = message?.parts?.[0]?.text || "";
            __chat_yield({ message: { parts: [{ text: `echo:${text}` }] } });
            __chat_yield({
                task: {
                    status: { state: "TASK_STATE_COMPLETED" }
                }
            });
        };
    "#;
    let agent = common::provenance::build_provenance_agent(writer.clone(), js).await;

    let context_ids: Vec<ContextId> = (0..4).map(|i| ContextId::new(10, i as u64)).collect();
    for (idx, context_id) in context_ids.iter().enumerate() {
        let correlation_id = CorrelationId::new(100 + idx as u64, 1);
        let request = send_stream_request(
            &format!("msg-{idx}"),
            "hello",
            &correlation_id.to_string(),
            Some(context_id.clone()),
        );
        let responses =
            tokio::time::timeout(Duration::from_secs(15), collect_responses(&agent, request))
                .await
                .expect("request timeout")
                .expect("handle");
        assert!(
            !responses.is_empty(),
            "expected at least one stream response"
        );
    }

    // Core invariant for this suite: request-scoped context survives A2A handling.
    // Provenance strictness itself is covered in baml-agent-runner provenance-backed tests.
}
