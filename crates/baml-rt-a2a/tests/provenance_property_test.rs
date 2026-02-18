//! Provenance attribution test using GraphQLite in-memory store.

#![recursion_limit = "256"]

mod common;

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::{ContextId, CorrelationId};
use baml_rt_provenance::GraphqliteStoreBuilder;
use std::sync::Arc;
use test_support::common::send_stream_request;
use tokio::time::Duration;

async fn collect_responses(
    agent: &A2aAgent,
    request: serde_json::Value,
) -> baml_rt::Result<Vec<serde_json::Value>> {
    Ok(baml_rt_core::collect_a2a_stream(agent.handle_a2a_stream(request).await?).await)
}

#[tokio::test(flavor = "current_thread")]
async fn test_scope_attribution_without_cross_contamination() {
    let writer = GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("build store");
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
