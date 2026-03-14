use serde_json::{Value, json};

use crate::tools::ToolRegistry;

const EVENT_LOG_TEXT_CAP: usize = 512;

#[derive(Debug, Clone)]
pub struct PromptProjectionItem {
    pub timestamp_ms: u64,
    pub event_id: String,
    pub role: String,
    pub source: String,
    pub content: Value,
}

fn content_to_string(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
}

fn compact_event_log_content(content: &str) -> String {
    if content.len() <= EVENT_LOG_TEXT_CAP {
        return content.to_string();
    }
    let mut out = content.chars().take(EVENT_LOG_TEXT_CAP).collect::<String>();
    out.push('…');
    out
}

pub fn project_prompt_context(
    context_id: &str,
    mut items: Vec<PromptProjectionItem>,
    tool_registry: &ToolRegistry,
) -> Value {
    for item in &mut items {
        if item.source != "tool_result" {
            continue;
        }
        let Some(tool_name) = item.content.get("tool_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(handler) = tool_registry.get_handler(tool_name) else {
            continue;
        };
        handler.compact_result(&mut item.content);
    }

    let mut conversation_history = Vec::with_capacity(items.len());
    let mut event_log = Vec::with_capacity(items.len());
    let mut message_count = 0_u64;
    let mut tool_call_count = 0_u64;
    let mut tool_result_count = 0_u64;
    let mut last_role: Option<String> = None;
    let mut last_source: Option<String> = None;
    let mut last_event_id: Option<String> = None;

    for item in items {
        let content = match &item.content {
            Value::String(s) => s.clone(),
            other => content_to_string(other),
        };
        match item.source.as_str() {
            "message" => message_count += 1,
            "tool_call" => tool_call_count += 1,
            "tool_result" => tool_result_count += 1,
            _ => {}
        }
        last_role = Some(item.role.clone());
        last_source = Some(item.source.clone());
        last_event_id = Some(item.event_id.clone());
        conversation_history.push(json!({
            "role": item.role,
            "source": item.source,
            "content": content,
        }));
        event_log.push(json!({
            "event_id": item.event_id,
            "timestamp_ms": item.timestamp_ms,
            "role": item.role,
            "source": item.source,
            "content": compact_event_log_content(&content),
        }));
    }

    let session_state = json!({
        "context_id": context_id,
        "total_events": conversation_history.len(),
        "message_count": message_count,
        "tool_call_count": tool_call_count,
        "tool_result_count": tool_result_count,
        "last_role": last_role,
        "last_source": last_source,
        "last_event_id": last_event_id,
    });

    json!({
        "conversation_history": conversation_history,
        "event_log": event_log,
        "session_state": session_state,
    })
}
