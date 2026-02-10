use baml_rt::a2a_types::{
    A2aMessageId, JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, ROLE_USER,
    SendMessageRequest,
};
use baml_rt_core::ids::{ContextId, ExternalId};
use serde_json::Value;
use std::collections::HashMap;

pub fn user_message(message_id: &str, text: &str, context_id: Option<ContextId>) -> Message {
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

pub fn send_stream_request(
    message_id: &str,
    text: &str,
    request_id: &str,
    context_id: Option<ContextId>,
) -> Value {
    let params = SendMessageRequest {
        message: user_message(message_id, text, context_id),
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

pub fn chunk_content(response: &Value) -> Option<&Value> {
    response
        .get("result")
        .and_then(|result| result.get("chunk").or(Some(result)))
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
    responses.iter().find_map(|response| {
        let content = chunk_content(response)?;
        content
            .get("task")
            .and_then(|task| task.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}
