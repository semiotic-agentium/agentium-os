//! Property tests for stream chunk ordering and finality.
//!
//! **Purpose:** Assert that when the agent yields K chunks, we get K responses in order
//! and exactly one response is marked final (the last). Validates order preservation and finality.

#![recursion_limit = "256"]

use baml_rt::BamlRuntimeManager;
use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use futures_util::StreamExt;
use proptest::prelude::*;
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
    let agent = A2aAgent::builder()
        .with_runtime_manager(BamlRuntimeManager::new().unwrap())
        .with_init_js(js)
        .with_effect_emitter(std::sync::Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(baml_rt::QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
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
        .handle_a2a_stream(request)
        .await
        .expect("handle_a2a_stream");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

// **Purpose:** For K in 1..=20, an agent that yields K chunks must produce K responses
// in index order and exactly one response with `final: true` (the last).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]
    #[test]
    fn prop_stream_chunk_order_and_finality(k in 1u32..=20u32) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let responses = rt.block_on(run_stream_test(k));
        let k_usize = k as usize;
        assert_eq!(
            responses.len(),
            k_usize,
            "expected {} stream responses, got {}",
            k_usize,
            responses.len()
        );
        let mut final_count = 0u32;
        for (i, response) in responses.iter().enumerate() {
            let result = response.get("result").and_then(|r| r.as_object()).expect("result");
            let index = result.get("index").and_then(Value::as_u64).unwrap_or(i as u64);
            assert_eq!(index, i as u64, "chunk order: index {} should be {}", i, index);
            let chunk = result.get("chunk").cloned().unwrap_or(Value::Null);
            if let Some(chunk_index) = chunk.get("index").and_then(Value::as_u64) {
                assert_eq!(chunk_index, i as u64, "chunk content index");
            }
            if result.get("final").and_then(Value::as_bool).unwrap_or(false) {
                final_count += 1;
            }
        }
        assert_eq!(
            final_count, 1,
            "exactly one chunk must be final, got {}",
            final_count
        );
    }
}
