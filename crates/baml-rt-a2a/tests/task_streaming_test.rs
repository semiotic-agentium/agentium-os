use async_trait::async_trait;
use baml_rt::a2a_types::{JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, SendMessageRequest};
use baml_rt::tools::BamlTool;
use baml_rt_tools::bundles::BundleType;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt::{A2aAgent, A2aRequestHandler};
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
use ts_rs::TS;
use std::collections::HashMap;
use test_support::common::CalculatorTool;

// Test bundle for test tools
struct Test;

impl BundleType for Test {
    const NAME: &'static str = "test";
    fn description() -> &'static str {
        "Test tools for unit testing"
    }
}

fn fixture_js_code() -> String {
    r#"
    globalThis.handle_a2a_request = async function(request) {
        const method = request?.method;
        const params = request?.params || {};
        const message = params.message || {};
        const text = message.parts?.[0]?.text || "";
        const messageId = message.messageId || "msg";
        const contextId = message.contextId || "ctx";

        if (method === "message.send") {
            if (text.startsWith("long-rite:")) {
                return {
                    task: {
                        id: `rite-task-${messageId}`,
                        contextId,
                        metadata: { agent: "test-agent" },
                        status: { state: "TASK_STATE_WORKING" },
                        history: []
                    }
                };
            }
            if (text.startsWith("tool-call:")) {
                const session = await openToolSession("test/add_numbers");
                await session.send({ a: 2, b: 3 });
                const step = await session.continue();
                const result = step && step.output ? step.output : {};
                return {
                    message: {
                        messageId: `resp-${messageId}`,
                        role: "ROLE_AGENT",
                        parts: [{ text: `sum=${result.result}` }]
                    }
                };
            }
            if (text.startsWith("baml-tool:")) {
                const session = await openToolSession("support/calculate");
                await session.send({ expression: { left: 2, operation: "Add", right: 3 } });
                const step = await session.continue();
                const result = step && step.output ? step.output : {};
                return {
                    message: {
                        messageId: `resp-${messageId}`,
                        role: "ROLE_AGENT",
                        parts: [{ text: `sum=${result.result}` }]
                    }
                };
            }
            return {
                message: {
                    messageId: `resp-${messageId}`,
                    role: "ROLE_AGENT",
                    parts: [{ text: "ok" }]
                }
            };
        }

        if (method === "message.sendStream") {
            return [
                { statusUpdate: { contextId, taskId: `rite-task-${messageId}`, status: { state: "TASK_STATE_WORKING" } } },
                { artifactUpdate: { contextId, taskId: `rite-task-${messageId}`, artifact: { name: "rite-log", parts: [{ text: "sealed" }] } } }
            ];
        }

        if (method === "tasks.subscribe") {
            const taskId = params.id || `rite-task-${messageId}`;
            return [
                { statusUpdate: { contextId, taskId, status: { state: "TASK_STATE_WORKING" } } },
                { artifactUpdate: { contextId, taskId, artifact: { name: "rite-log", parts: [{ text: "sealed" }] } } }
            ];
        }

        return {
            message: {
                messageId: `resp-${messageId}`,
                role: "ROLE_AGENT",
                parts: [{ text: "unknown" }]
            }
        };
    };
    "#
    .to_string()
}

fn user_message(message_id: &str, text: &str) -> Message {
    use baml_rt_core::ids::{ContextId, ExternalId};
    use baml_rt_a2a::a2a_types::A2aMessageId;
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(message_id)),
        role: MessageRole::String("ROLE_USER".to_string()),
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: Some(ContextId::new(1, 1)),
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

async fn setup_agent() -> A2aAgent {
    let mut manager = BamlRuntimeManager::new().unwrap();
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(fixture_js_code())
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn test_message_send_deterministic_task() {
    let agent = setup_agent().await;
    let params = SendMessageRequest {
        message: user_message("vox-1", "long-rite: reactor benediction"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.send".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-3-1".to_string())),
    };

    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let result = responses[0].get("result").cloned().unwrap_or(Value::Null);
    let task_id = result
        .get("task")
        .and_then(|task| task.get("id"))
        .and_then(|value| value.as_str());
    assert_eq!(task_id, Some("rite-task-vox-1"));
}

#[tokio::test]
async fn test_message_send_stream_emits_updates() {
    let agent = setup_agent().await;
    let params = SendMessageRequest {
        message: user_message("vox-2", "ignite the void seals"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-3-2".to_string())),
    };

    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();

    let mut saw_status = false;
    let mut saw_artifact = false;
    for response in responses {
        if let Some(chunk) = response
            .get("result")
            .and_then(|result| result.get("chunk"))
        {
            if chunk.get("statusUpdate").is_some() {
                saw_status = true;
            }
            if chunk.get("artifactUpdate").is_some() {
                saw_artifact = true;
            }
        }
    }

    assert!(saw_status, "expected a statusUpdate stream chunk");
    assert!(saw_artifact, "expected an artifactUpdate stream chunk");
}

#[tokio::test]
async fn test_tasks_subscribe_streams_incremental_updates() {
    let agent = setup_agent().await;
    let params = SendMessageRequest {
        message: user_message("vox-3", "long-rite: plasma canticle"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let create_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.send".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-3-3".to_string())),
    };
    let _ = agent
        .handle_a2a(serde_json::to_value(create_request).unwrap())
        .await
        .unwrap();

    let stream_params = SendMessageRequest {
        message: user_message("vox-3", "ignite the void seals"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let stream_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(stream_params).unwrap()),
        id: Some(JSONRPCId::String("corr-3-4".to_string())),
    };
    let _ = agent
        .handle_a2a(serde_json::to_value(stream_request).unwrap())
        .await
        .unwrap();

    let subscribe_request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "tasks.subscribe".to_string(),
        params: Some(json!({ "id": "rite-task-vox-3", "stream": true })),
        id: Some(JSONRPCId::String("corr-3-5".to_string())),
    };
    let responses = agent
        .handle_a2a(serde_json::to_value(subscribe_request).unwrap())
        .await
        .unwrap();

    let mut saw_status = false;
    let mut saw_artifact = false;
    for response in responses {
        if let Some(chunk) = response
            .get("result")
            .and_then(|result| result.get("chunk"))
        {
            if chunk.get("statusUpdate").is_some() {
                saw_status = true;
            }
            if chunk.get("artifactUpdate").is_some() {
                saw_artifact = true;
            }
        }
    }

    assert!(saw_status, "expected status updates in subscribe stream");
    assert!(saw_artifact, "expected artifact updates in subscribe stream");
}

struct AddNumbersTool;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct AddNumbersInput {
    a: f64,
    b: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
struct AddNumbersOutput {
    result: f64,
}

#[async_trait]
impl BamlTool for AddNumbersTool {
    type Bundle = Test;
    const LOCAL_NAME: &'static str = "add_numbers";
    type OpenInput = ();
    type Input = AddNumbersInput;
    type Output = AddNumbersOutput;

    fn description(&self) -> &'static str {
        "Adds two numbers together"
    }

    async fn execute(&self, args: Self::Input) -> baml_rt::Result<Self::Output> {
        Ok(AddNumbersOutput { result: args.a + args.b })
    }
}

#[tokio::test]
async fn test_message_send_tool_calling() {
    let agent = setup_agent().await;
    {
        let runtime = agent.runtime();
        let mut manager = runtime.lock().await;
        manager.register_tool(AddNumbersTool).await.unwrap();
    }

    let params = SendMessageRequest {
        message: user_message("vox-4", "tool-call: add numbers"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.send".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-3-6".to_string())),
    };

    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let result = responses[0].get("result").cloned().unwrap_or(Value::Null);
    let text = result
        .get("message")
        .and_then(|message| message.get("parts"))
        .and_then(|parts| parts.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(
        text.contains("sum=5"),
        "expected tool result in message text, got: {}",
        text
    );
}

#[tokio::test]
async fn test_message_send_baml_tool_calling() {
    let agent = setup_agent().await;
    {
        let runtime = agent.runtime();
        let mut manager = runtime.lock().await;
        manager.register_tool(CalculatorTool).await.unwrap();
    }

    let params = SendMessageRequest {
        message: user_message("vox-5", "baml-tool: rite of sums"),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.send".to_string(),
        params: Some(serde_json::to_value(params).unwrap()),
        id: Some(JSONRPCId::String("corr-3-7".to_string())),
    };

    let responses = agent
        .handle_a2a(serde_json::to_value(request).unwrap())
        .await
        .unwrap();
    let result = responses[0].get("result").cloned().unwrap_or(Value::Null);
    let text = result
        .get("message")
        .and_then(|message| message.get("parts"))
        .and_then(|parts| parts.as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(
        text.contains("sum=5"),
        "expected BAML tool result in message text, got: {}",
        text
    );
}
