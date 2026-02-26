//! Stream ordering/finality smoke checks.
//!
//! High-entropy interleaving properties live in `run_handle_a2a_property_test`.
//! This file keeps a narrow deterministic smoke check for chunk ordering/finality
//! in the synthetic `count:N` stream fixture path.

#![recursion_limit = "256"]

mod common;

use baml_rt::BamlRuntimeManager;
use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::A2aWireRequest;
use futures_util::StreamExt;
use serde_json::Value;
use test_support::common::send_stream_request;

fn js_yield_n_chunks() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";
        const match = /^count:(\d+)$/.exec(text.trim());
        const n = match ? Math.min(parseInt(match[1], 10), 50) : 1;
        for (let i = 0; i < n; i++) {
            __chat_yield({
                index: i,
                total: n,
                task: {
                    status: {
                        state: i + 1 === n ? "TASK_STATE_COMPLETED" : "TASK_STATE_WORKING"
                    }
                }
            });
        }
    };
    "#
    .to_string()
}

async fn run_stream_test(k: u32) -> Vec<Value> {
    let js = js_yield_n_chunks();
    let store = common::provenance::build_graphqlite_test_store();
    let agent = A2aAgent::builder()
        .with_runtime_manager(BamlRuntimeManager::new().unwrap())
        .with_init_js(js)
        .with_effect_emitter(std::sync::Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(baml_rt::QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_graphqlite_store(store)
        .build()
        .await
        .unwrap();

    let request = send_stream_request(
        "msg-1",
        &format!("count:{}", k),
        "corr-1700000000100-1",
        None,
    );
    let mut stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await
        .expect("handle_a2a_stream");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.into_inner());
    }
    out
}

/// Content chunks are those with result.chunk.index (agent yields). The stream may also
/// include injected SUBMITTED and a terminal ChannelClosed; we assert only on content chunks.
fn assert_stream_chunk_order_and_finality(responses: &[Value], k: u32) {
    let content_responses: Vec<_> = responses
        .iter()
        .filter(|r| {
            r.get("result")
                .and_then(|res| res.get("chunk"))
                .and_then(|c| c.get("index"))
                .is_some()
        })
        .collect();
    let k_usize = k as usize;
    assert_eq!(
        content_responses.len(),
        k_usize,
        "expected {} content (agent-yield) stream responses, got {}; total responses={}",
        k_usize,
        content_responses.len(),
        responses.len()
    );
    for (i, response) in content_responses.iter().enumerate() {
        let result = response
            .get("result")
            .and_then(|r| r.as_object())
            .expect("result");
        let chunk = result.get("chunk").cloned().unwrap_or(Value::Null);
        if let Some(chunk_index) = chunk.get("index").and_then(Value::as_u64) {
            assert_eq!(
                chunk_index, i as u64,
                "chunk order: content index {} should be {}",
                i, chunk_index
            );
        }
    }
    let total_final = responses
        .iter()
        .filter(|r| {
            r.get("result")
                .and_then(|res| res.get("final"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        total_final, 1,
        "exactly one stream response must be final, got {}",
        total_final
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_stream_chunk_order_and_finality_smoke() {
    for k in [1u32, 3u32, 20u32] {
        let responses = run_stream_test(k).await;
        assert_stream_chunk_order_and_finality(&responses, k);
    }
}
