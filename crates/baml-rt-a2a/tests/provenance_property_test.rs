//! Property tests for scope attribution (provenance context_id).
//!
//! **Purpose:** Assert that under concurrent A2A requests, every provenance event’s
//! `context_id` belongs to the set of request context_ids we sent—no cross-contamination.
//!
//! **Invariant:** ∀ concurrent requests with distinct context_ids, ∀ provenance event e:
//! `e.context_id ∈ {context_ids of the request that produced e}`.

use async_trait::async_trait;
use baml_rt::tools::BamlTool;
use baml_rt::{BamlRuntimeManager, QuickJSConfig};
use baml_rt_a2a::a2a_types::{
    JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, ROLE_USER, SendMessageRequest,
};
use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_core::ids::ContextId;
use baml_rt_provenance::InMemoryProvenanceStore;
use baml_rt_tools::bundles::BundleType;
use proptest::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use ts_rs::TS;

struct Test;
impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for property tests"
    }
}

fn user_message(msg_id: &str, text: &str, context_id: ContextId) -> Message {
    use baml_rt_a2a::a2a_types::A2aMessageId;
    use baml_rt_core::ids::ExternalId;
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(msg_id)),
        role: MessageRole::String(ROLE_USER.to_string()),
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: Some(context_id),
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: std::collections::HashMap::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct EchoInput {
    text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct EchoOutput {
    echo: String,
}

struct EchoTool;

#[async_trait]
impl BamlTool for EchoTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "echo_tool";
    type OpenInput = ();
    type Input = EchoInput;
    type Output = EchoOutput;

    fn description(&self) -> &'static str {
        "Echo for scope property tests"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(EchoOutput { echo: args.text })
    }
}

async fn build_agent(writer: Arc<InMemoryProvenanceStore>) -> A2aAgent {
    let js = r#"
        globalThis.onChatMessage = async function(message) {
            const text = message?.parts?.[0]?.text || "";
            const session = await openToolSession("test/echo_tool", __baml_invocation_token);
            await session.send({ text });
            const step = await session.continue();
            const out = step && step.output ? step.output : {};
            __baml_chat_yield(out);
        };
    "#;
    let mut runtime = BamlRuntimeManager::new().expect("runtime");
    runtime.register_tool(EchoTool).await.expect("register");
    A2aAgent::builder()
        .with_provenance_writer(writer)
        .with_runtime_manager(runtime)
        .with_init_js(js)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .expect("agent build")
}

// **Purpose:** For 2..=8 concurrent sendStream requests with distinct context_ids, every
// provenance event’s context_id must be in that set (no event attributed to another request).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(4))]
    #[test]
    fn prop_scope_attribution_no_cross_contamination(num_requests in 2u32..=8u32) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let writer = Arc::new(InMemoryProvenanceStore::new());
            let agent = build_agent(writer.clone()).await;
            let context_ids: Vec<ContextId> = (0..num_requests)
                .map(|i| ContextId::new(10, i as u64))
                .collect();
            let id_set: HashSet<ContextId> = context_ids.iter().cloned().collect();

            let local = tokio::task::LocalSet::new();
            local.run_until(async {
                let mut handles = Vec::new();
                for (idx, context_id) in context_ids.iter().enumerate() {
                    let agent_clone = agent.clone();
                    let request = JSONRPCRequest {
                        jsonrpc: "2.0".to_string(),
                        method: "message.sendStream".to_string(),
                        params: Some(
                            serde_json::to_value(SendMessageRequest {
                                message: user_message(
                                    &format!("msg-{}", idx),
                                    "hello",
                                    context_id.clone(),
                                ),
                                configuration: None,
                                metadata: None,
                                tenant: None,
                                extra: std::collections::HashMap::new(),
                            })
                            .expect("params"),
                        ),
                        id: Some(JSONRPCId::String(format!("corr-{}", idx + 100))),
                    };
                    let request_value = serde_json::to_value(request).expect("request");
                    handles.push(tokio::task::spawn_local(async move {
                        let _ = agent_clone.handle_a2a(request_value).await.expect("handle");
                    }));
                }
                for h in handles {
                    h.await.expect("join");
                }
            })
            .await;

            let events = writer.events().await;
            for event in events.iter() {
                let cid = event.context_id();
                assert!(
                    id_set.contains(cid),
                    "event context_id {} must be one of the request context_ids (no cross-contamination)",
                    cid
                );
            }
        });
    }
}
