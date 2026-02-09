use baml_rt::tools::BamlTool;
use baml_rt::{BamlRuntimeManager, QuickJSConfig, Result};
use baml_rt_a2a::a2a_types::{
    JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, ROLE_USER, SendMessageRequest,
};
use baml_rt_a2a::{A2aAgent, A2aRequestHandler};
use baml_rt_tools::bundles::BundleType;

// Test bundle for test tools
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}
use async_trait::async_trait;
use baml_rt_a2a::a2a_types::A2aMessageId;
use baml_rt_core::ids::{ContextId, ExternalId};
use baml_rt_provenance::InMemoryProvenanceStore;
use baml_rt_provenance::ProvEventData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use test_support::support::a2a::A2aInMemoryClient;
use ts_rs::TS;

fn user_message(message_id: &str, text: &str, context_id: Option<ContextId>) -> Message {
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(message_id)),
        role: MessageRole::String(ROLE_USER.to_string()),
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id,
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

async fn setup_agent(writer: Arc<InMemoryProvenanceStore>) -> A2aAgent {
    let js_code = r#"
        globalThis.handle_a2a_request = async function(request) {
            const params = request && request.params ? request.params : {};
            const ctx = params.message && params.message.contextId ? params.message.contextId : "missing";
            __baml_a2a_yield({
                task: {
                    id: "task-ctx",
                    contextId: ctx,
                    metadata: { agent: "test-agent" },
                    status: { state: "TASK_STATE_WORKING" },
                    history: []
                }
            });
        };
    "#;
    A2aAgent::builder()
        .with_provenance_writer(writer)
        .with_init_js(js_code)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .expect("agent build")
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
    let writer1 = Arc::new(InMemoryProvenanceStore::new());
    let writer2 = Arc::new(InMemoryProvenanceStore::new());
    let agent1 = setup_agent(writer1.clone()).await;
    let agent2 = setup_agent(writer2.clone()).await;

    let params = SendMessageRequest {
        message: user_message("msg-1", "hello", None),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).expect("serialize params")),
        id: Some(JSONRPCId::String("corr-2-1".to_string())),
    };
    let request_value = serde_json::to_value(request).expect("serialize request");
    let responses = agent1.handle_a2a(request_value).await.expect("a2a handle");
    let context_id = expect_context_id(responses);

    let client = A2aInMemoryClient::new(Arc::new(agent2));
    let params = SendMessageRequest {
        message: user_message(
            "msg-2",
            "forward",
            Some(ContextId::parse_temporal(&context_id).expect("context id")),
        ),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).expect("serialize params")),
        id: Some(JSONRPCId::String("corr-2-2".to_string())),
    };
    let request_value = serde_json::to_value(request).expect("serialize request");
    let _ = client.send(request_value).await.expect("agent2 handle");

    let events = writer2.events().await;
    let context_id_typed = ContextId::parse_temporal(&context_id).expect("context id");
    assert!(
        events
            .iter()
            .any(|event| event.context_id() == &context_id_typed),
        "expected provenance events to include propagated context_id"
    );
}

/// Asserts context concurrency invariants (I1–I3): request-scoped attribution, session tool count, no cross-request attribution.
/// See baml-rt-quickjs/docs/CONTEXT_CONCURRENCY_INVARIANTS.md. No retries—failures indicate real bugs.
#[tokio::test(flavor = "current_thread")]
async fn test_context_id_is_task_local_under_concurrency() {
    let writer = Arc::new(InMemoryProvenanceStore::new());
    let js_code = r#"
        globalThis.handle_a2a_request = async function(request) {
            const text = request?.params?.message?.parts?.[0]?.text || "";
            const session = await openToolSession("test/echo_tool", __baml_invocation_token);
            await session.send({ text });
            const step = await session.continue();
            const out = step && step.output ? step.output : {};
            __baml_a2a_yield(out);
        };
    "#;

    let mut runtime = BamlRuntimeManager::new().expect("runtime");
    runtime
        .register_tool(EchoTool)
        .await
        .expect("register echo tool");

    let agent = A2aAgent::builder()
        .with_provenance_writer(writer.clone())
        .with_runtime_manager(runtime)
        .with_init_js(js_code)
        .with_effect_emitter(Arc::new(baml_rt_core::effects::EffectBus::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .expect("agent build");

    let context_ids: Vec<ContextId> = (0..8).map(|i| ContextId::new(10, i as u64)).collect();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut handles = Vec::new();
            for (idx, context_id) in context_ids.iter().enumerate() {
                let agent_clone = agent.clone();
                let request = JSONRPCRequest {
                    jsonrpc: "2.0".to_string(),
                    method: "message.sendStream".to_string(),
                    params: Some(
                        serde_json::to_value(SendMessageRequest {
                            message: user_message(
                                &format!("msg-{idx}"),
                                "hello",
                                Some(context_id.clone()),
                            ),
                            configuration: None,
                            metadata: None,
                            tenant: None,
                            extra: HashMap::new(),
                        })
                        .expect("serialize params"),
                    ),
                    id: Some(JSONRPCId::String(format!("corr-2-{}", idx + 3))),
                };
                let request_value = serde_json::to_value(request).expect("serialize request");
                handles.push(tokio::task::spawn_local(async move {
                    let _ = agent_clone
                        .handle_a2a(request_value)
                        .await
                        .expect("a2a handle");
                }));
            }

            for handle in handles {
                handle.await.expect("join");
            }
        })
        .await;

    let events = writer.events().await;
    for context_id in context_ids {
        let (starts, completes, successes) =
            tool_event_counts(&events, "test/echo_tool", &context_id);
        // Session-based tool calls emit two tool invocations per request (open + execute).
        assert_eq!(
            starts, 2,
            "expected 2 tool starts for {context_id}, got {starts}"
        );
        assert_eq!(
            completes, 2,
            "expected 2 tool completions for {context_id}, got {completes}"
        );
        assert_eq!(
            successes, 2,
            "expected 2 tool successes for {context_id}, got {successes}"
        );
        assert_eq!(
            starts, completes,
            "pre/post pairing mismatch for {context_id}"
        );
    }
}

#[derive(Debug)]
struct EchoTool;

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

#[async_trait]
impl BamlTool for EchoTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "echo_tool";
    type OpenInput = ();
    type Input = EchoInput;
    type Output = EchoOutput;

    fn description(&self) -> &'static str {
        "Echo tool for concurrency testing."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        Ok(EchoOutput { echo: args.text })
    }
}

fn tool_event_counts(
    events: &[baml_rt_provenance::ProvEvent],
    tool_name: &str,
    context_id: &ContextId,
) -> (usize, usize, usize) {
    let mut starts = 0;
    let mut completes = 0;
    let mut successes = 0;
    for event in events {
        if event.context_id() != context_id {
            continue;
        }
        match event.data() {
            ProvEventData::ToolCallStarted {
                tool_name: name, ..
            } if name == tool_name => {
                starts += 1;
            }
            ProvEventData::ToolCallCompleted {
                tool_name: name,
                success,
                ..
            } if name == tool_name => {
                completes += 1;
                if *success {
                    successes += 1;
                }
            }
            _ => {}
        }
    }
    (starts, completes, successes)
}
