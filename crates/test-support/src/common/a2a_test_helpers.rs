use std::collections::HashMap;

use baml_rt::a2a_types::{
    A2aMessageId, JSONRPCId, JSONRPCRequest, Message, MessageRole, Part, SendMessageRequest,
};
use baml_rt_core::{
    A2aStreamChunk,
    bus::BusStream,
    ids::{ContextId, ExternalId, TaskId},
};
use futures_util::StreamExt;
use serde_json::Value;

/// Drives a `BusStream<A2aStreamChunk>` chunk-by-chunk and returns the first
/// `Some(_)` the predicate yields. The stream is dropped on match.
///
/// Use for tests asserting one signal ("FSM hit COMPLETED", "this text
/// appeared", "any chunk arrived"). For tests asserting on chunk
/// sequences, fold the running state into the predicate (return
/// `Some(())` only when a running tally hits the expected order); the
/// predicate state should stay bounded — accumulating chunks into a
/// `Vec` inside the closure re-creates the buffer-everything shape this
/// helper exists to avoid.
pub async fn await_first_match<F, T>(
    mut stream: BusStream<A2aStreamChunk>,
    mut predicate: F,
) -> Option<T>
where
    F: FnMut(&Value) -> Option<T>,
{
    while let Some(chunk) = stream.next().await {
        if let Some(result) = predicate(chunk.as_ref()) {
            return Some(result);
        }
    }
    None
}

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

/// Matches a JSON-RPC response envelope carrying `result.chunk.task.status.state == TASK_STATE_INPUT_REQUIRED`.
pub fn response_has_input_required(response: &Value) -> Option<()> {
    let state = response
        .get("result")
        .and_then(|r| r.get("chunk"))
        .and_then(|c| c.get("task"))
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)?;
    (state == "TASK_STATE_INPUT_REQUIRED").then_some(())
}

/// Matches a JSON-RPC response envelope carrying the terminal `result.final == true` marker.
pub fn response_has_final_chunk(response: &Value) -> Option<()> {
    response
        .get("result")
        .and_then(|r| r.get("final"))
        .and_then(Value::as_bool)
        .filter(|is_final| *is_final)
        .map(|_| ())
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

/// Collects wire `Message` JSON values embedded in a stream chunk (top-level `message`,
/// `task.history` / `task.status.message`, flattened `status.message`, `statusUpdate`, …).
fn message_surfaces_from_chunk(chunk: &Value) -> Vec<&Value> {
    let mut out: Vec<&Value> = Vec::new();
    if let Some(m) = chunk.get("message") {
        out.push(m);
    }
    if let Some(t) = chunk.get("task") {
        if let Some(hist) = t.get("history").and_then(Value::as_array) {
            for m in hist {
                out.push(m);
            }
        }
        if let Some(m) = t.get("status").and_then(|s| s.get("message")) {
            out.push(m);
        }
    }
    // Flattened task/status-update fields on the chunk (serde flatten / relay shapes).
    if let Some(m) = chunk.get("status").and_then(|s| s.get("message")) {
        out.push(m);
    }
    if let Some(su) = chunk.get("statusUpdate") {
        if let Some(m) = su.get("status").and_then(|s| s.get("message")) {
            out.push(m);
        }
        let nested = su.get("statusUpdate").or_else(|| su.get("status_update"));
        if let Some(inner) = nested {
            if let Some(m) = inner.get("message") {
                out.push(m);
            }
            if let Some(m) = inner.get("status").and_then(|s| s.get("message")) {
                out.push(m);
            }
        }
    }
    out
}

/// Parts from `artifactUpdate.artifact` (or bare `artifact`) on a stream chunk.
fn artifact_parts_from_chunk(chunk: &Value) -> Vec<&Value> {
    let artifact = chunk
        .get("artifactUpdate")
        .and_then(|u| u.get("artifact"))
        .or_else(|| chunk.get("artifact"));
    artifact
        .and_then(|a| a.get("parts"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

fn message_parts(message: &Value) -> Vec<Value> {
    if let Some(parts) = message.get("parts").and_then(Value::as_array) {
        return parts.clone();
    }
    let Some(raw) = message.as_str() else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    parsed
        .get("parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Serialises a wire `Part.data` field for test assertions.
///
/// A2A `Part.data` is typed as arbitrary JSON (`serde_json::Value`). The chat shim may emit
/// parsed objects (see `structuredReplyToWireMessage`), not only JSON strings — callers that
/// only matched `Value::as_str` would miss substantive model output carried in `data`.
fn part_data_as_searchable_string(data: &Value) -> Option<String> {
    match data {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn part_visible_string(part: &Value) -> Option<String> {
    if let Some(t) = part.get("text").and_then(Value::as_str)
        && !t.trim().is_empty()
    {
        return Some(t.to_string());
    }
    part.get("data").and_then(part_data_as_searchable_string)
}

/// Extracts message text from the first part of each chunk.
pub fn message_texts_from_chunks(chunks: &[&Value]) -> Vec<String> {
    chunks
        .iter()
        .flat_map(|chunk| {
            let from_messages = message_surfaces_from_chunk(chunk)
                .into_iter()
                .flat_map(message_parts);
            let from_artifact = artifact_parts_from_chunk(chunk).into_iter().cloned();
            from_messages.chain(from_artifact)
        })
        .filter_map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .filter(|s| !s.is_empty())
                .or_else(|| part.get("data").and_then(part_data_as_searchable_string))
        })
        .collect()
}

/// Extracts user-visible content from every part of every chunk — both `TextPart.text`
/// and `DataPart.data` (JSON string **or** structured JSON per A2A `Part.data: Value`).
/// Use this when an assertion needs to see the agent's full response, since some prompts
/// legitimately route detail into `DataPart` rather than text.
pub fn message_visible_content_from_chunks(chunks: &[&Value]) -> Vec<String> {
    chunks
        .iter()
        .flat_map(|chunk| {
            let from_messages = message_surfaces_from_chunk(chunk)
                .into_iter()
                .flat_map(message_parts);
            let from_artifact = artifact_parts_from_chunk(chunk).into_iter().cloned();
            from_messages.chain(from_artifact)
        })
        .filter_map(|part| part_visible_string(&part))
        .collect()
}

pub fn first_message_text_from_stream(responses: &[Value]) -> String {
    for response in responses {
        let Some(content) = chunk_content(response) else {
            continue;
        };
        for surface in message_surfaces_from_chunk(content) {
            for part in message_parts(surface) {
                if let Some(s) = part_visible_string(&part) {
                    return s;
                }
            }
        }
        for part in artifact_parts_from_chunk(content) {
            if let Some(s) = part_visible_string(part) {
                return s;
            }
        }
    }
    String::new()
}

pub fn first_task_id_from_stream(responses: &[Value]) -> Option<TaskId> {
    fn task_id_from_val(task: &Value) -> Option<TaskId> {
        task.get("id")
            .and_then(Value::as_str)
            .map(|id| TaskId::from_external(ExternalId::new(id.to_string())))
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
