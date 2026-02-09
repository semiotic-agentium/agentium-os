//! Property tests for stream chunk ordering and finality.
//!
//! **Purpose:** Assert that when the agent yields K chunks, we get K responses in order
//! and exactly one response is marked final (the last). Validates order preservation and finality.

use baml_rt::BamlRuntimeManager;
use baml_rt_a2a::a2a_types::{
    JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, ROLE_USER, SendMessageRequest,
};
use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use proptest::prelude::*;
use serde_json::Value;
use std::collections::HashMap;

fn js_yield_n_chunks() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";
        const match = /^count:(\d+)$/.exec(text.trim());
        const n = match ? Math.min(parseInt(match[1], 10), 50) : 1;
        for (let i = 0; i < n; i++) {
            __baml_chat_yield({ index: i, total: n });
        }
    };
    "#
    .to_string()
}

fn user_message(msg_id: &str, text: &str) -> Message {
    use baml_rt_a2a::a2a_types::A2aMessageId;
    use baml_rt_core::ids::ExternalId;
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(msg_id)),
        role: MessageRole::String(ROLE_USER.to_string()),
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: None,
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

async fn run_stream_test(k: u32) -> Vec<Value> {
    let js = js_yield_n_chunks();
    let agent = A2aAgent::builder()
        .with_runtime_manager(BamlRuntimeManager::new().unwrap())
        .with_init_js(js)
        .with_effect_emitter(std::sync::Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(baml_rt::QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .unwrap();

    let params = SendMessageRequest {
        message: user_message("msg-1", &format!("count:{}", k)),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-stream-1".to_string())),
    };
    agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .expect("handle_a2a")
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
            let chunk_index = chunk.get("index").and_then(Value::as_u64).unwrap_or(0);
            assert_eq!(chunk_index, i as u64, "chunk content index");
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
