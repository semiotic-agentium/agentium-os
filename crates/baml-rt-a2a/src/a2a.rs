// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Minimal A2A JSON-RPC request/response helpers.
//!
//! This provides a thin adapter layer without adding external dependencies.

use baml_rt_core::{
    BamlRtError, InvocationKind, Result, context,
    context::{InvocationScope, RequestScope},
    ids::{ContextId, DerivedId, ExternalId, MessageId, TaskId},
    stream_completion::StreamCompletion,
    to_json_value,
};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    a2a_types::{
        A2aMessageId, GetTaskRequest, JSONRPCError, JSONRPCErrorResponse, JSONRPCId,
        JSONRPCRequest, JSONRPCSuccessResponse, ListTasksRequest, Message, ROLE_AGENT,
        SendMessageRequest, StreamResponse, SubscribeToTaskRequest, Task,
    },
    wire::{A2aWireMethod, A2aWireRequest},
};

const JSONRPC_VERSION: &str = "2.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A2aMethod {
    MessageSendStream,
    TasksGet,
    TasksList,
    TasksSubscribe,
}

impl A2aMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            A2aMethod::MessageSendStream => "message.sendStream",
            A2aMethod::TasksGet => "tasks.get",
            A2aMethod::TasksList => "tasks.list",
            A2aMethod::TasksSubscribe => "tasks.subscribe",
        }
    }
}

#[derive(Debug, Clone)]
pub struct A2aRequest {
    pub id: Option<JSONRPCId>,
    pub params: A2aParams,
    pub invocation: InvocationKind,
    /// Request-level scope (message vs task); agent_id is added at transport.
    pub resolved_scope: RequestScope,
}

impl A2aRequest {
    pub fn from_value(value: Value) -> Result<Self> {
        let raw: JSONRPCRequest = serde_json::from_value(value).map_err(BamlRtError::Json)?;
        if raw.jsonrpc != JSONRPC_VERSION {
            return Err(BamlRtError::InvalidArgument(format!(
                "Unsupported jsonrpc version: {version}",
                version = raw.jsonrpc
            )));
        }
        let request = A2aWireRequest::try_from_raw(raw)?;

        let id = request.id;
        let (params, invocation, resolved_scope) = match request.method {
            A2aWireMethod::MessageSendStream { params } => {
                let mut params = *params;
                if params.message.context_id.is_none() {
                    let generated = context::generate_context_id();
                    tracing::debug!(
                        context_id = %generated,
                        message_id = %params.message.message_id.as_message_id(),
                        "A2aRequest::from_value: generated context_id for MessageSendStream (client omitted)"
                    );
                    params.message.context_id = Some(generated);
                }
                let context_id = params.message.context_id.clone().expect("set above");
                let message_id = params.message.message_id.as_message_id().clone();
                let scope = match params.message.task_id.clone() {
                    Some(task_id) => RequestScope::TaskScoped {
                        context_id: context_id.clone(),
                        message_id: message_id.clone(),
                        task_id,
                    },
                    None => RequestScope::MessageScoped {
                        context_id,
                        message_id,
                    },
                };
                (
                    A2aParams::MessageSendStream(Box::new(params)),
                    InvocationKind::Stream,
                    scope,
                )
            }
            A2aWireMethod::TasksGet { params } => {
                let task_id = params.id.clone();
                let context_id = context::generate_context_id();
                let message_id = MessageId::from_external(ExternalId::new(format!(
                    "tasks.get-{}",
                    task_id.as_str()
                )));
                let scope = RequestScope::TaskScoped {
                    context_id,
                    message_id,
                    task_id,
                };
                (A2aParams::TasksGet(params), InvocationKind::Invoke, scope)
            }
            A2aWireMethod::TasksList { params } => {
                let params: ListTasksRequest = params.into();
                let context_id = params
                    .context_id
                    .clone()
                    .unwrap_or_else(context::generate_context_id);
                let message_id =
                    MessageId::from_external(ExternalId::new("tasks.list".to_string()));
                let scope = RequestScope::MessageScoped {
                    context_id,
                    message_id,
                };
                (A2aParams::TasksList(params), InvocationKind::Invoke, scope)
            }
            A2aWireMethod::TasksSubscribe { params } => {
                let stream_flag = params.stream.unwrap_or(false);
                let request: SubscribeToTaskRequest = params.into();
                let task_id = request.id.clone();
                let context_id = context::generate_context_id();
                let message_id = MessageId::from_external(ExternalId::new(format!(
                    "tasks.subscribe-{}",
                    task_id.as_str()
                )));
                let scope = RequestScope::TaskScoped {
                    context_id,
                    message_id,
                    task_id,
                };
                (
                    A2aParams::TasksSubscribe(request),
                    InvocationKind::from(stream_flag),
                    scope,
                )
            }
        };

        Ok(Self {
            id,
            params,
            invocation,
            resolved_scope,
        })
    }

    pub fn correlation_id(&self) -> Option<String> {
        self.id.as_ref().map(id_to_string)
    }

    pub fn context_id(&self) -> &ContextId {
        self.resolved_scope.context_id()
    }

    pub fn message_id(&self) -> &MessageId {
        self.resolved_scope.message_id()
    }

    /// Task id when request is task-scoped (resume, tasks.get, tasks.subscribe).
    pub fn task_id_opt(&self) -> Option<&TaskId> {
        self.resolved_scope.task_id_opt()
    }

    pub fn method(&self) -> A2aMethod {
        self.params.method()
    }

    pub fn is_stream(&self) -> bool {
        self.invocation.is_stream()
    }
}

/// Receiver for incremental stream chunks: (normalized_chunk, index, completion).
pub type StreamReceiver = mpsc::Receiver<(StreamResponse, usize, Option<StreamCompletion>)>;

/// Handle for a stream outcome: receiver for chunks and optional resume sender for true resume
/// (live sessions that suspend on InputRequired). When present, transport sends the next turn
/// request on `resume_tx` instead of starting a new invocation.
#[derive(Debug)]
pub struct StreamHandle {
    pub receiver: StreamReceiver,
    /// When present, next message for same context_id is sent here to resume the same JS run.
    pub resume_tx: Option<mpsc::Sender<Value>>,
    /// When present, send a unit value to request immediate upstream stream abort.
    pub abort_tx: Option<mpsc::Sender<()>>,
}

#[derive(Debug)]
pub enum A2aOutcome {
    Response(Value),
    Stream(StreamHandle),
}

#[derive(Debug, Clone)]
pub enum A2aParams {
    MessageSendStream(Box<SendMessageRequest>),
    TasksGet(GetTaskRequest),
    TasksList(ListTasksRequest),
    TasksSubscribe(SubscribeToTaskRequest),
}

impl A2aParams {
    pub fn method(&self) -> A2aMethod {
        match self {
            A2aParams::MessageSendStream(_) => A2aMethod::MessageSendStream,
            A2aParams::TasksGet(_) => A2aMethod::TasksGet,
            A2aParams::TasksList(_) => A2aMethod::TasksList,
            A2aParams::TasksSubscribe(_) => A2aMethod::TasksSubscribe,
        }
    }

    pub fn as_send_message(&self) -> Option<&SendMessageRequest> {
        match self {
            A2aParams::MessageSendStream(params) => Some(params),
            _ => None,
        }
    }

    pub fn to_value(&self) -> Result<Value> {
        match self {
            A2aParams::MessageSendStream(params) => to_json_value(params),
            A2aParams::TasksGet(params) => to_json_value(params),
            A2aParams::TasksList(params) => to_json_value(params),
            A2aParams::TasksSubscribe(params) => to_json_value(params),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFinality {
    Final,
    NotFinal,
}

impl StreamFinality {
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }
}

pub fn success_response(id: Option<JSONRPCId>, result: Value) -> Value {
    serde_json::to_value(JSONRPCSuccessResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        result,
        id,
    })
    .unwrap_or_else(|_| {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": null,
            "result": { "error": "serialization failed" }
        })
    })
}

pub fn error_response(
    id: Option<JSONRPCId>,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> Value {
    let error = JSONRPCError {
        code: code as i32,
        message: message.to_string(),
        data,
    };
    serde_json::to_value(JSONRPCErrorResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        error,
        id,
    })
    .unwrap_or_else(|_| {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": null,
            "error": { "code": -32603, "message": "serialization failed" }
        })
    })
}

pub fn stream_chunk_response(
    id: Option<JSONRPCId>,
    chunk: Value,
    index: usize,
    finality: StreamFinality,
) -> Value {
    serde_json::to_value(JSONRPCSuccessResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        result: json!({
            "stream": true,
            "index": index,
            "final": finality.is_final(),
            "chunk": chunk,
        }),
        id,
    })
    .unwrap_or_else(|_| {
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": null,
            "result": { "error": "serialization failed" }
        })
    })
}

fn id_to_string(value: &JSONRPCId) -> String {
    match value {
        JSONRPCId::String(s) => s.clone(),
        JSONRPCId::Integer(n) => n.to_string(),
        JSONRPCId::Null => "null".to_string(),
    }
}

pub fn extract_jsonrpc_id(value: &Value) -> Option<JSONRPCId> {
    serde_json::from_value::<JSONRPCRequest>(value.clone())
        .ok()
        .and_then(|request| request.id)
}

pub fn extract_agent_name(value: &Value) -> Option<String> {
    let raw: JSONRPCRequest = serde_json::from_value(value.clone()).ok()?;
    let request = A2aWireRequest::try_from_raw(raw).ok()?;
    let params = match request.method {
        A2aWireMethod::MessageSendStream { params } => *params,
        _ => return None,
    };
    metadata_value_as_string(params.metadata.as_ref(), "agent")
        .or_else(|| metadata_value_as_string(params.metadata.as_ref(), "agent_name"))
        .or_else(|| metadata_value_as_string(params.message.metadata.as_ref(), "agent"))
        .or_else(|| metadata_value_as_string(params.message.metadata.as_ref(), "agent_name"))
}

/// BAML function name for compaction policy resolution from message.sendStream metadata.
#[must_use]
pub fn compaction_function_name_from_request(request: &A2aRequest) -> Option<String> {
    let A2aParams::MessageSendStream(params) = &request.params else {
        return None;
    };
    compaction_function_name_from_send_params(params)
}

fn compaction_function_name_from_send_params(params: &SendMessageRequest) -> Option<String> {
    metadata_value_as_string(params.metadata.as_ref(), "baml_function")
        .or_else(|| metadata_value_as_string(params.metadata.as_ref(), "function_name"))
        .or_else(|| metadata_value_as_string(params.metadata.as_ref(), "bamlFunction"))
        .or_else(|| metadata_value_as_string(params.message.metadata.as_ref(), "baml_function"))
        .or_else(|| metadata_value_as_string(params.message.metadata.as_ref(), "function_name"))
        .or_else(|| metadata_value_as_string(params.message.metadata.as_ref(), "bamlFunction"))
}

fn metadata_value_as_string(
    metadata: Option<&std::collections::HashMap<String, Value>>,
    key: &str,
) -> Option<String> {
    metadata
        .and_then(|meta| meta.get(key))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

#[derive(Debug, Clone)]
pub struct JsChunkNormalizer {
    context_id: ContextId,
    task_id: TaskId,
    message_counter: u64,
}

impl JsChunkNormalizer {
    pub fn new(scope: &InvocationScope) -> Self {
        let task_id = scope.task_id_opt().cloned().unwrap_or_else(|| {
            TaskId::for_js_runtime(baml_rt_core::ids::UuidId::new(Uuid::new_v4()))
        });
        Self {
            context_id: scope.context_id().clone(),
            task_id,
            message_counter: 0,
        }
    }

    /// Single normalization pass: ensure stream-chunk shape (wrap bare Message/Task) then fill scope-derived fields.
    pub fn normalize_value(&mut self, value: Value) -> Result<Value> {
        let is_wrapped = value
            .as_object()
            .map(|m| {
                m.contains_key("message")
                    || m.contains_key("task")
                    || m.contains_key("statusUpdate")
                    || m.contains_key("artifactUpdate")
            })
            .unwrap_or(false);

        let mut value = if !is_wrapped {
            if let Ok(message) = serde_json::from_value::<Message>(value.clone()) {
                json!({ "message": to_json_value(&message)? })
            } else if let Ok(task) = serde_json::from_value::<Task>(value.clone()) {
                json!({ "task": to_json_value(&task)? })
            } else if value.as_object().and_then(|m| m.get("parts")).is_some() {
                let mut message_value = value;
                self.ensure_message_fields(&mut message_value)?;
                return Ok(json!({ "message": message_value }));
            } else {
                value
            }
        } else {
            value
        };

        if let Some(map) = value.as_object_mut() {
            // Contract: status_update or artifact_update requires task in chunk. Inject task from scope when missing.
            if (map.contains_key("statusUpdate") || map.contains_key("artifactUpdate"))
                && !map.contains_key("task")
            {
                let mut task = json!({
                    "id": self.task_id.as_str(),
                    "contextId": self.context_id.as_str(),
                });
                self.ensure_task_fields(&mut task)?;
                map.insert("task".to_string(), task);
            }
            if let Some(message) = map.get_mut("message") {
                self.ensure_message_fields(message)?;
            }
            if let Some(task) = map.get_mut("task") {
                self.ensure_task_fields(task)?;
            }
            if let Some(status_update) = map.get_mut("statusUpdate") {
                self.ensure_status_update_fields(status_update)?;
            }
            if let Some(artifact_update) = map.get_mut("artifactUpdate") {
                self.ensure_artifact_update_fields(artifact_update)?;
            }
        }
        Ok(value)
    }

    fn next_message_id(&mut self) -> String {
        self.message_counter += 1;
        let derived = DerivedId::new(format!(
            "js-msg-{context_id}-{counter}",
            context_id = self.context_id.as_str(),
            counter = self.message_counter
        ));
        A2aMessageId::outgoing(derived)
            .as_message_id()
            .as_str()
            .to_string()
    }

    fn ensure_message_fields(&mut self, message: &mut Value) -> Result<()> {
        let Some(map) = message.as_object_mut() else {
            return Ok(());
        };
        map.entry("messageId".to_string())
            .or_insert_with(|| Value::String(self.next_message_id()));
        map.entry("role".to_string())
            .or_insert_with(|| Value::String(ROLE_AGENT.to_string()));
        map.entry("contextId".to_string())
            .or_insert_with(|| Value::String(self.context_id.as_str().to_string()));
        map.entry("taskId".to_string())
            .or_insert_with(|| Value::String(self.task_id.as_str().to_string()));
        Ok(())
    }

    fn ensure_task_fields(&mut self, task: &mut Value) -> Result<()> {
        let Some(map) = task.as_object_mut() else {
            return Ok(());
        };
        map.entry("id".to_string())
            .or_insert_with(|| Value::String(self.task_id.as_str().to_string()));
        map.entry("contextId".to_string())
            .or_insert_with(|| Value::String(self.context_id.as_str().to_string()));
        if let Some(history) = map.get_mut("history").and_then(Value::as_array_mut) {
            for message in history {
                self.ensure_message_fields(message)?;
            }
        }
        if let Some(status) = map.get_mut("status") {
            self.ensure_status_fields(status)?;
        }
        Ok(())
    }

    fn ensure_status_fields(&mut self, status: &mut Value) -> Result<()> {
        let Some(map) = status.as_object_mut() else {
            return Ok(());
        };
        if let Some(message) = map.get_mut("message") {
            self.ensure_message_fields(message)?;
        }
        Ok(())
    }

    fn ensure_context_and_task_fields(&self, map: &mut Map<String, Value>) {
        map.entry("contextId".to_string())
            .or_insert_with(|| Value::String(self.context_id.as_str().to_string()));
        map.entry("taskId".to_string())
            .or_insert_with(|| Value::String(self.task_id.as_str().to_string()));
    }

    fn ensure_status_update_fields(&mut self, status_update: &mut Value) -> Result<()> {
        let Some(map) = status_update.as_object_mut() else {
            return Ok(());
        };
        self.ensure_context_and_task_fields(map);
        if let Some(status) = map.get_mut("status") {
            self.ensure_status_fields(status)?;
        }
        Ok(())
    }

    fn ensure_artifact_update_fields(&mut self, artifact_update: &mut Value) -> Result<()> {
        let Some(map) = artifact_update.as_object_mut() else {
            return Ok(());
        };
        self.ensure_context_and_task_fields(map);
        Ok(())
    }
}

/// Value passed to JS handler. For message.sendStream we pass parts and routing
/// identifiers so JS session keys can resume the correct conversation.
/// Includes messageId so agents can emit task ids that align with the request (e.g. task-{messageId})
/// and tasks.subscribe can resolve the same task.
pub fn request_to_js_value(request: &A2aRequest) -> Result<Value> {
    match request.params.as_send_message() {
        Some(params) => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "messageId".to_string(),
                Value::String(
                    params
                        .message
                        .message_id
                        .as_message_id()
                        .as_str()
                        .to_string(),
                ),
            );
            obj.insert("parts".to_string(), json!(params.message.parts));
            if let Some(ref cid) = params.message.context_id {
                obj.insert(
                    "contextId".to_string(),
                    Value::String(cid.as_str().to_string()),
                );
            }
            if let Some(ref tid) = params.message.task_id {
                obj.insert(
                    "taskId".to_string(),
                    Value::String(tid.as_str().to_string()),
                );
            }
            Ok(Value::Object(obj))
        }
        None => request.params.to_value(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use baml_rt_core::{BamlRtError, Result};
    use serde_json::{Value, json};
    use tokio::time::{Duration, timeout};

    use super::A2aRequest;
    use crate::{
        A2aAgent, A2aRequestHandler,
        a2a_types::{
            JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, SendMessageRequest,
            StreamChunkView,
        },
    };

    /// Watchdog timeout for agent setup and tests - ensures tests fail fast if they hang.
    const TEST_WATCHDOG_TIMEOUT_SECS: u64 = 30;

    /// Setup may consume up to [`TEST_WATCHDOG_TIMEOUT_SECS`]; tests that chain several RPCs
    /// after setup need a larger outer budget so parallel `cargo test` does not exhaust the litany.
    const TEST_WATCHDOG_MULTI_STEP_SECS: u64 = 60;

    async fn setup_agent_with_js() -> A2aAgent {
        timeout(
            Duration::from_secs(TEST_WATCHDOG_TIMEOUT_SECS),
            setup_agent_with_js_inner(),
        )
        .await
        .expect("Agent setup timed out - builder hung")
    }

    async fn setup_agent_with_js_inner() -> A2aAgent {
        tracing::info!("setup_agent_with_js_inner: Starting agent setup");
        let js_code = r#"
            globalThis.onChatMessage = async function(message) {
                const text = (message && message.parts && message.parts[0] && message.parts[0].text) || "friend";
                if (text === "task") {
                    __chat_yield({
                        task: {
                            metadata: { agent: "test-agent" },
                            status: { state: "TASK_STATE_WORKING" }
                        },
                        final: true
                    });
                    return;
                }
                __chat_yield({ message: { parts: [{ text: "hi " + text }] } });
                __chat_yield({ message: { parts: [{ text: "done" }] }, final: true });
            };
        "#;
        tracing::info!("setup_agent_with_js_inner: Creating builder");
        let store = baml_rt_provenance::SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("test store");
        let builder = A2aAgent::builder()
            .with_init_js(js_code)
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_surreal_store(store)
            .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub());
        tracing::info!("setup_agent_with_js_inner: Calling build()");
        let agent = builder.build().await.expect("agent build");
        tracing::info!("setup_agent_with_js_inner: Agent built successfully");
        agent
    }

    fn expect_success_result(responses: Vec<Value>) -> Value {
        let response = responses.into_iter().next().expect("response");
        if let Some(error) = response.get("error") {
            panic!("unexpected error response: {error}");
        }
        let result = response.get("result").cloned().expect("missing result");
        // Stream responses wrap each chunk as result: { chunk, final? }; unwrap to chunk content
        result.get("chunk").cloned().unwrap_or(result)
    }

    /// From a stream of responses, return the first chunk that contains a message (result.chunk.message).
    /// Streams may include status updates (SUBMITTED, WORKING) before message chunks; this test
    /// asserts the assistant's first message content (e.g. "hi Ada").
    fn expect_first_message_chunk(responses: Vec<Value>) -> Value {
        let response = responses
            .into_iter()
            .find(|r| {
                r.get("error").is_none()
                    && r.get("result")
                        .and_then(|res| res.get("chunk").or(Some(res)))
                        .and_then(|c| c.get("message"))
                        .is_some()
            })
            .expect("stream should include a chunk with message");
        let result = response.get("result").cloned().expect("missing result");
        result.get("chunk").cloned().unwrap_or(result)
    }

    async fn collect_responses(agent: &A2aAgent, request: Value) -> Result<Vec<Value>> {
        let stream = agent
            .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
            .await?;
        let chunks = baml_rt_core::collect_a2a_stream_one_shot(stream).await;
        Ok(chunks
            .into_iter()
            .map(baml_rt_core::A2aStreamChunk::into_inner)
            .collect())
    }

    /// Resolve task id from `message.sendStream` JSON-RPC chunks (live path may emit SUBMITTED /
    /// statusUpdate / `task` without nested `id` — same rules as [`StreamChunkView`]).
    fn task_id_from_stream_responses(responses: &[Value]) -> Option<String> {
        responses.iter().find_map(|response| {
            if response.get("error").is_some() {
                return None;
            }
            let result = response.get("result")?;
            let chunk = result
                .get("chunk")
                .cloned()
                .or_else(|| Some(result.clone()))?;
            StreamChunkView::new(chunk)
                .task_id()
                .map(|t| t.as_str().to_string())
        })
    }

    fn user_message(message_id: &str, text: &str) -> Message {
        use baml_rt_core::ids::ExternalId;

        use crate::a2a_types::A2aMessageId;
        Message {
            message_id: A2aMessageId::incoming(ExternalId::new(message_id)),
            role: MessageRole::User,
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

    #[tokio::test]
    async fn test_a2a_jsonrpc_request_invokes_js_function() {
        timeout(
            Duration::from_secs(TEST_WATCHDOG_TIMEOUT_SECS),
            test_a2a_jsonrpc_request_invokes_js_function_inner(),
        )
        .await
        .expect("Test timed out - test hung");
    }

    async fn test_a2a_jsonrpc_request_invokes_js_function_inner() {
        let agent = setup_agent_with_js().await;

        let params = SendMessageRequest {
            message: user_message("msg-1", "Ada"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: HashMap::new(),
        };
        let request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "message.sendStream".to_string(),
            params: Some(serde_json::to_value(params).expect("serialize params")),
            id: Some(JSONRPCId::String("corr-1-10".to_string())),
        };
        let request_value = serde_json::to_value(request).expect("serialize request");

        let responses = collect_responses(&agent, request_value)
            .await
            .expect("a2a handle");
        let result = expect_first_message_chunk(responses);
        let message = result
            .get("message")
            .and_then(Value::as_object)
            .expect("response message");
        let parts = message
            .get("parts")
            .and_then(Value::as_array)
            .expect("message parts");
        let text = parts
            .first()
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str);
        assert_eq!(text, Some("hi Ada"));
    }

    #[tokio::test]
    async fn test_a2a_stream_suffix_dispatches_stream() {
        timeout(
            Duration::from_secs(TEST_WATCHDOG_TIMEOUT_SECS),
            test_a2a_stream_suffix_dispatches_stream_inner(),
        )
        .await
        .expect("Test timed out - test hung");
    }

    async fn test_a2a_stream_suffix_dispatches_stream_inner() {
        let agent = setup_agent_with_js().await;

        let params = SendMessageRequest {
            message: user_message("msg-2", "Ada"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: HashMap::new(),
        };
        let request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "message.sendStream".to_string(),
            params: Some(serde_json::to_value(params).expect("serialize params")),
            id: Some(JSONRPCId::String("corr-1-11".to_string())),
        };
        let request_value = serde_json::to_value(request).expect("serialize request");

        let responses = collect_responses(&agent, request_value)
            .await
            .expect("a2a handle");
        assert!(!responses.is_empty(), "stream should return chunks");
        let any_final = responses.iter().any(|value| {
            value
                .get("result")
                .and_then(|result| result.get("final"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        assert!(any_final, "stream should include a final chunk");
    }

    #[tokio::test]
    async fn test_a2a_stream_param_dispatches_stream() {
        timeout(
            Duration::from_secs(TEST_WATCHDOG_TIMEOUT_SECS),
            test_a2a_stream_param_dispatches_stream_inner(),
        )
        .await
        .expect("Test timed out - test hung");
    }

    async fn test_a2a_stream_param_dispatches_stream_inner() {
        let agent = setup_agent_with_js().await;

        let params = SendMessageRequest {
            message: user_message("msg-3", "Ada"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: HashMap::new(),
        };
        let mut params_value = serde_json::to_value(params).expect("serialize params");
        if let Value::Object(ref mut map) = params_value {
            map.insert("stream".to_string(), Value::Bool(true));
        }
        let request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "message.sendStream".to_string(),
            params: Some(params_value),
            id: Some(JSONRPCId::String("corr-1-12".to_string())),
        };
        let request_value = serde_json::to_value(request).expect("serialize request");

        let responses = collect_responses(&agent, request_value)
            .await
            .expect("a2a handle");
        assert!(!responses.is_empty(), "stream should return chunks");
        let any_final = responses.iter().any(|value| {
            value
                .get("result")
                .and_then(|result| result.get("final"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        assert!(any_final, "stream should include a final chunk");
    }

    /// tasks.get and tasks.list must see a task created by message.sendStream (live path).
    #[tokio::test]
    async fn test_tasks_get_list_cancel() {
        timeout(
            Duration::from_secs(TEST_WATCHDOG_MULTI_STEP_SECS),
            test_tasks_get_list_cancel_inner(),
        )
        .await
        .expect("Test timed out - test hung");
    }

    async fn test_tasks_get_list_cancel_inner() {
        let agent = setup_agent_with_js().await;

        let params = SendMessageRequest {
            message: user_message("msg-task", "task"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: HashMap::new(),
        };
        let create_request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "message.sendStream".to_string(),
            params: Some(serde_json::to_value(params).expect("serialize params")),
            id: Some(JSONRPCId::String("corr-1-13".to_string())),
        };
        let create_value = serde_json::to_value(create_request).expect("serialize request");
        let create_responses = collect_responses(&agent, create_value)
            .await
            .expect("create task");
        let task_id =
            task_id_from_stream_responses(&create_responses).expect("task id from stream");

        let get_request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks.get".to_string(),
            params: Some(json!({ "id": task_id })),
            id: Some(JSONRPCId::String("corr-1-14".to_string())),
        };
        let responses = collect_responses(
            &agent,
            serde_json::to_value(get_request).expect("serialize request"),
        )
        .await
        .expect("get task");
        let result = expect_success_result(responses);
        assert_eq!(
            result.get("id").and_then(Value::as_str),
            Some(task_id.as_str())
        );

        let list_request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks.list".to_string(),
            params: Some(json!({})),
            id: Some(JSONRPCId::String("corr-1-15".to_string())),
        };
        let responses = collect_responses(
            &agent,
            serde_json::to_value(list_request).expect("serialize request"),
        )
        .await
        .expect("list tasks");
        let result = expect_success_result(responses);
        let tasks = result
            .get("tasks")
            .and_then(Value::as_array)
            .expect("tasks list");
        assert!(
            tasks
                .iter()
                .any(|task| task.get("id").and_then(Value::as_str) == Some(task_id.as_str()))
        );
    }

    /// tasks.subscribe stream must include a final chunk; task must be persisted after message.sendStream.
    #[tokio::test]
    async fn test_tasks_subscribe_stream() {
        timeout(
            Duration::from_secs(TEST_WATCHDOG_MULTI_STEP_SECS),
            test_tasks_subscribe_stream_inner(),
        )
        .await
        .expect("Test timed out - test hung");
    }

    async fn test_tasks_subscribe_stream_inner() {
        let agent = setup_agent_with_js().await;

        let params = SendMessageRequest {
            message: user_message("msg-task-stream", "task"),
            configuration: None,
            metadata: None,
            tenant: None,
            extra: HashMap::new(),
        };
        let create_request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "message.sendStream".to_string(),
            params: Some(serde_json::to_value(params).expect("serialize params")),
            id: Some(JSONRPCId::String("corr-1-17".to_string())),
        };
        let create_value = serde_json::to_value(create_request).expect("serialize request");
        let create_responses = collect_responses(&agent, create_value)
            .await
            .expect("create task");
        let task_id =
            task_id_from_stream_responses(&create_responses).expect("task id from stream");

        let subscribe_request = JSONRPCRequest {
            jsonrpc: "2.0".to_string(),
            method: "tasks.subscribe".to_string(),
            params: Some(json!({ "id": task_id, "stream": true })),
            id: Some(JSONRPCId::String("corr-1-18".to_string())),
        };
        let responses = collect_responses(
            &agent,
            serde_json::to_value(subscribe_request).expect("serialize request"),
        )
        .await
        .expect("subscribe task");
        assert!(
            !responses.is_empty(),
            "subscribe should return a stream response"
        );
        let any_final = responses.iter().any(|value| {
            value
                .get("result")
                .and_then(|result| result.get("final"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        assert!(any_final, "subscribe stream should include a final chunk");
    }

    /// Proves the typed-params refactor preserves the JS payload for message.sendStream.
    /// The argument-sketch E2E test depends on JS receiving `{ "parts": [...] }`.
    #[test]
    fn test_request_to_js_value_preserves_parts_payload() {
        use super::request_to_js_value;

        let request_json = json!({
            "jsonrpc": "2.0",
            "id": "corr-proof",
            "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-proof",
                    "role": "ROLE_USER",
                    "parts": [
                        { "text": "No we didn't." },
                        { "text": "Second part." }
                    ]
                }
            }
        });

        let parsed = A2aRequest::from_value(request_json).expect("parse request");
        let js_value = request_to_js_value(&parsed).expect("js value");

        let parts = js_value
            .get("parts")
            .and_then(Value::as_array)
            .expect("js payload should have parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0].get("text").and_then(Value::as_str),
            Some("No we didn't.")
        );
        assert_eq!(
            parts[1].get("text").and_then(Value::as_str),
            Some("Second part.")
        );

        // Verify required payload fields are present, and only known routing keys can be added.
        let obj = js_value.as_object().expect("js payload should be object");
        assert!(
            obj.contains_key("parts"),
            "JS payload should always include 'parts'"
        );
        let allowed = ["parts", "contextId", "taskId", "messageId"];
        assert!(obj.keys().all(|k| allowed.contains(&k.as_str())));
    }

    /// Verifies that metadata fields (method, context_id, invocation) are correctly
    /// derived from typed params after the wire-types refactor.
    #[test]
    fn test_from_value_populates_metadata_from_typed_params() {
        use baml_rt_core::InvocationKind;

        use super::A2aMethod;

        let request_json = json!({
            "jsonrpc": "2.0",
            "id": "corr-meta",
            "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-meta",
                    "role": "ROLE_USER",
                    "parts": [{ "text": "hello" }],
                    "contextId": "ctx-42"
                }
            }
        });

        let parsed = A2aRequest::from_value(request_json).expect("parse request");
        assert_eq!(parsed.method(), A2aMethod::MessageSendStream);
        assert_eq!(parsed.invocation, InvocationKind::Stream);
        assert_eq!(parsed.context_id().as_str(), "ctx-42");
        assert!(!parsed.message_id().as_str().is_empty());
    }

    #[test]
    fn test_a2a_jsonrpc_version_validation() {
        let request = json!({
            "jsonrpc": "1.0",
            "id": "bad-1",
            "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-4",
                    "role": "ROLE_USER",
                    "parts": [{ "text": "Ada" }]
                }
            }
        });

        let err = A2aRequest::from_value(request).expect_err("should reject bad version");
        match err {
            BamlRtError::InvalidArgument(_) | BamlRtError::Json(_) => {}
            other => panic!("unexpected error: {}", other),
        }
    }

    #[test]
    fn test_message_role_alias_assistant_maps_to_agent() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": "role-alias-1",
            "method": "message.sendStream",
            "params": {
                "message": {
                    "messageId": "msg-role-alias",
                    "role": "assistant",
                    "parts": [{ "text": "hello" }]
                }
            }
        });

        let parsed = A2aRequest::from_value(request).expect("role alias should parse");
        let params = parsed
            .params
            .as_send_message()
            .expect("message.sendStream params");
        assert_eq!(params.message.role, MessageRole::Agent);
    }

    #[test]
    fn test_message_role_rejects_empty_or_unknown() {
        for bad_role in ["", "narrator", "ROLE_UNKNOWN"] {
            let request = json!({
                "jsonrpc": "2.0",
                "id": "role-bad-1",
                "method": "message.sendStream",
                "params": {
                    "message": {
                        "messageId": "msg-bad-role",
                        "role": bad_role,
                        "parts": [{ "text": "hello" }]
                    }
                }
            });

            let err = A2aRequest::from_value(request).expect_err("invalid role should reject");
            match err {
                BamlRtError::Json(_) | BamlRtError::InvalidArgument(_) => {}
                other => panic!("unexpected error: {}", other),
            }
        }
    }
}
