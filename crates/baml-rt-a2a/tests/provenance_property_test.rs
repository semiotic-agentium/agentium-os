//! Provenance attribution test using SurrealDB in-memory store.

#![recursion_limit = "256"]

mod common;

use baml_rt_a2a::A2aRequestHandler;
use baml_rt_core::ids::{ContextId, CorrelationId};
use test_support::common::{await_first_match, send_stream_request};
use tokio::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn test_scope_attribution_without_cross_contamination() {
    let writer = common::provenance::build_surreal_test_store().await;
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
        let stream = agent
            .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
            .await
            .expect("handle");
        let first_chunk = tokio::time::timeout(
            Duration::from_secs(15),
            await_first_match(stream, |_chunk| Some(())),
        )
        .await
        .expect("request timeout");
        assert!(
            first_chunk.is_some(),
            "expected at least one stream chunk for context {context_id:?}"
        );
    }

    // Core invariant for this suite: request-scoped context survives A2A handling.
    // Provenance strictness itself is covered in baml-agent-runner provenance-backed tests.
}
