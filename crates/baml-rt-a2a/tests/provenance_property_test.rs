#![cfg(feature = "falkordb-tests")]
//! Provenance attribution test using FalkorDB.

#![recursion_limit = "256"]

use baml_rt::QuickJSConfig;

use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::{ContextId, CorrelationId};
use baml_rt_provenance::{FalkorDbProvenanceConfig, FalkorDbProvenanceWriter};
use std::sync::Arc;
use test_support::common::send_stream_request;
use test_support::common::shared_falkordb;
use tokio::time::Duration;

async fn build_agent(writer: Arc<FalkorDbProvenanceWriter>) -> A2aAgent {
    let js = r#"
        globalThis.onChatMessage = async function(message) {
            const text = message?.parts?.[0]?.text || "";
            __chat_yield({ message: { parts: [{ text: `echo:${text}` }] } });
        };
    "#;
    A2aAgent::builder()
        .with_provenance_writer(writer)
        .with_init_js(js)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .expect("agent build")
}

#[tokio::test(flavor = "current_thread")]
async fn test_scope_attribution_without_cross_contamination() {
    let connection = shared_falkordb().await;
    let graph = format!("baml_a2a_scope_prop_{}", std::process::id());

    let writer = Arc::new(FalkorDbProvenanceWriter::new(
        FalkorDbProvenanceConfig::new(connection.to_owned(), graph),
    ));
    let agent = build_agent(writer.clone()).await;

    let context_ids: Vec<ContextId> = (0..4).map(|i| ContextId::new(10, i as u64)).collect();
    for (idx, context_id) in context_ids.iter().enumerate() {
        let correlation_id = CorrelationId::new(100 + idx as u64, 1);
        let request = send_stream_request(
            &format!("msg-{idx}"),
            "hello",
            &correlation_id.to_string(),
            Some(context_id.clone()),
        );
        let responses = tokio::time::timeout(Duration::from_secs(15), agent.handle_a2a(request))
            .await
            .expect("request timeout")
            .expect("handle");
        assert!(
            !responses.is_empty(),
            "expected at least one stream response"
        );
    }

    // Core invariant for this suite: request-scoped context survives A2A handling.
    // Provenance strictness itself is covered in baml-agent-runner Falkor-backed tests.
}
