//! Protocol helpers for A2A stdio loop and plaintext wrapping.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use baml_rt_a2a::{
    a2a,
    a2a_types::{
        A2aMessageId, JSONRPCId, JSONRPCRequest, Message, MessageRole, Part,
        SendMessageConfiguration, SendMessageRequest,
    },
};
use baml_rt_core::{
    BamlRtError, ContextId, Result, context,
    ids::{DerivedId, ExternalId, TaskId},
};
use serde_json::Value;

use crate::agent_package::BootedAgent;

pub(crate) static MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(1);
static STDIO_CONTEXT_ID: std::sync::OnceLock<ContextId> = std::sync::OnceLock::new();
static STDIO_TASK_ID: std::sync::OnceLock<TaskId> = std::sync::OnceLock::new();

pub(crate) fn stdio_context_id() -> ContextId {
    STDIO_CONTEXT_ID
        .get_or_init(context::generate_context_id)
        .clone()
}

pub(crate) fn stdio_task_id() -> TaskId {
    STDIO_TASK_ID
        .get_or_init(|| {
            let context_id = stdio_context_id();
            TaskId::from_external(ExternalId::new(format!(
                "cli-task-{context_id}",
                context_id = context_id.as_str()
            )))
        })
        .clone()
}

pub(crate) fn wrap_plaintext_message(text: &str) -> Result<Value> {
    let seq = MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let message_id = A2aMessageId::outgoing(DerivedId::new(format!("cli-msg-{seq}")));
    let message = Message {
        message_id,
        role: MessageRole::User,
        parts: vec![Part {
            text: Some(text.to_string()),
            ..Part::default()
        }],
        context_id: Some(stdio_context_id()),
        task_id: Some(stdio_task_id()),
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    };
    let params = SendMessageRequest {
        message,
        configuration: Some(SendMessageConfiguration {
            blocking: Some(false),
            ..Default::default()
        }),
        metadata: None,
        tenant: None,
        extra: HashMap::new(),
    };
    let request = JSONRPCRequest {
        jsonrpc: "2.0".to_string(),
        method: "message.sendStream".to_string(),
        params: Some(serde_json::to_value(params).unwrap_or(Value::Null)),
        id: Some(JSONRPCId::Null),
    };
    serde_json::to_value(request)
        .map_err(|e| BamlRtError::InvalidArgument(format!("Failed to build stdio request: {e}")))
}

pub(crate) fn strip_stream_suffix(method: &str) -> (String, bool) {
    for suffix in ["/stream", ".stream", ":stream"] {
        if let Some(stripped) = method.strip_suffix(suffix) {
            return (stripped.to_string(), true);
        }
    }
    (method.to_string(), false)
}

pub(crate) fn split_agent_method(
    method: &str,
    agents: &HashMap<String, BootedAgent>,
) -> Option<(String, String)> {
    for sep in ["::", "/", "."] {
        if let Some((prefix, suffix)) = method.split_once(sep)
            && agents.contains_key(prefix)
        {
            return Some((prefix.to_string(), suffix.to_string()));
        }
    }
    None
}

pub(crate) fn select_implicit_stdio_agent(agents: &HashMap<String, BootedAgent>) -> Option<String> {
    if agents.len() == 1 {
        return agents.keys().next().cloned();
    }
    if agents.contains_key("coordinator-agent") {
        return Some("coordinator-agent".to_string());
    }
    None
}

pub(crate) fn is_a2a_method(method: &str) -> bool {
    method.starts_with("message/") || method.starts_with("tasks/") || method.starts_with("agent/")
}

/// Serialize an A2A JSON-RPC response for stdio; on failure returns a minimal error JSON line.
pub(crate) fn serialize_a2a_response(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
}

pub(crate) fn map_a2a_error(id: Option<JSONRPCId>, err: BamlRtError) -> Value {
    match err {
        BamlRtError::AgentNotFound(message) => {
            a2a::error_response(id, -32601, "Agent not found", Some(Value::String(message)))
        }
        BamlRtError::InvalidArgument(message) => {
            a2a::error_response(id, -32602, "Invalid params", Some(Value::String(message)))
        }
        BamlRtError::FunctionNotFound(message) => {
            a2a::error_response(id, -32601, "Method not found", Some(Value::String(message)))
        }
        BamlRtError::QuickJs(message) => {
            a2a::error_response(id, -32000, "QuickJS error", Some(Value::String(message)))
        }
        other => a2a::error_response(
            id,
            -32603,
            "Internal error",
            Some(Value::String(other.to_string())),
        ),
    }
}

pub(crate) fn unix_timestamp_secs() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_secs().to_string()
}
