use std::collections::HashMap;

use baml_rt::a2a_types::{
    A2aMessageId, JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, SendMessageRequest,
};
use baml_rt_core::ids::{ContextId, ExternalId, TaskId};
use serde_json::Value;

pub fn user_message(message_id: &str, text: &str, context_id: Option<ContextId>) -> Message {
    user_message_with_task(message_id, text, context_id, None)
}

/// Like user_message but allows setting message.task_id (used so scope has task_id and relay sends WORKING).
pub fn user_message_with_task(
    message_id: &str,
    text: &str,
    context_id: Option<ContextId>,
    task_id: Option<TaskId>,
) -> Message {
    Message {
        message_id: A2aMessageId::incoming(ExternalId::new(message_id)),
        role: MessageRole::User,
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id,
        task_id,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

pub fn send_stream_request(
    message_id: &str,
    text: &str,
    request_id: &str,
    context_id: Option<ContextId>,
) -> Value {
    send_stream_request_with_task(message_id, text, request_id, context_id, None)
}

/// Like send_stream_request but sets message.task_id so the scope has a task and the relay can send WORKING with task_id.
pub fn send_stream_request_with_task(
    message_id: &str,
    text: &str,
    request_id: &str,
    context_id: Option<ContextId>,
    task_id: Option<TaskId>,
) -> Value {
    let params = SendMessageRequest {
        message: user_message_with_task(message_id, text, context_id, task_id),
        configuration: None,
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).expect("serialize SendMessageRequest")),
        id: Some(JSONRPCId::String(request_id.to_string())),
    };
    serde_json::to_value(request).expect("serialize JSONRPCRequest")
}

/// Returns true if the JSON-RPC response envelope contains an error field.
pub fn is_error_response(response: &Value) -> bool {
    response.get("error").is_some()
}

pub fn chunk_content(response: &Value) -> Option<&Value> {
    response
        .get("result")
        .and_then(|result| result.get("chunk").or(Some(result)))
}

/// Extracts chunk contents from A2A stream responses (result.chunk or result).
pub fn chunks_from_responses(responses: &[Value]) -> Vec<&Value> {
    responses.iter().filter_map(chunk_content).collect()
}

/// Extracts message text from the first part of each chunk.
pub fn message_texts_from_chunks(chunks: &[&Value]) -> Vec<String> {
    chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .get("message")
                .and_then(|m| m.get("parts"))
                .and_then(|p| p.as_array())
                .and_then(|p| p.first())
                .and_then(|part| part.get("text"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
        .collect()
}

pub fn first_message_text_from_stream(responses: &[Value]) -> String {
    for response in responses {
        let Some(content) = chunk_content(response) else {
            continue;
        };
        let text = content
            .get("message")
            .and_then(|m| m.get("parts"))
            .and_then(|parts| parts.as_array())
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !text.is_empty() {
            return text.to_string();
        }
    }
    String::new()
}

pub fn first_task_id_from_stream(responses: &[Value]) -> Option<String> {
    fn task_id_from_val(task: &Value) -> Option<String> {
        task.get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                task.as_str().and_then(|s| {
                    serde_json::from_str::<Value>(s)
                        .ok()
                        .and_then(|v| task_id_from_val(&v))
                })
            })
    }
    responses.iter().find_map(|response| {
        let content = chunk_content(response)?;
        content.get("task").and_then(task_id_from_val)
    })
}
