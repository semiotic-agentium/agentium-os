//! Conversation-history UI resume hints derived from `a2a_task.status_json`.
//!
//! Provenance conversation rows do not encode `TASK_STATE_INPUT_REQUIRED`; the task subgraph
//! mirrors current task status. The runner merges these hints into the conversation-history page
//! DTO so clients can restore the awaiting-input affordance after a full snapshot reload.

use serde_json::Value;

use crate::{
    error::{ProvenanceError, Result},
    surreal_store::SurrealProvenanceStore,
    surreal_tables::TBL_A2A_TASK,
};

const INPUT_REQUIRED: &str = "TASK_STATE_INPUT_REQUIRED";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationResumeUiHints {
    /// Task id to echo when the HTTP request omitted `task_id` (latest task for the context).
    pub effective_task_id: Option<String>,
    pub awaiting_input: bool,
    pub input_required_prompt: Option<String>,
}

fn task_state_display(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => n.as_i64().map(|i| i.to_string()),
        _ => None,
    }
}

fn prompt_from_status_message(msg: &Value) -> Option<String> {
    let parts = msg.get("parts")?.as_array()?;
    for p in parts {
        if let Some(text) = p.get("text").and_then(|t| t.as_str()) {
            let t = text.trim();
            if !t.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// Resolve resume hints: `request_task_id` wins; otherwise the latest `a2a_task` row for
/// `context_id` (by monotonic `ord`).
pub async fn resolve_resume_ui_hints(
    store: &SurrealProvenanceStore,
    context_id: &str,
    request_task_id: Option<&str>,
) -> Result<ConversationResumeUiHints> {
    let map_err = |e: surrealdb::Error| ProvenanceError::Storage(Box::new(e));

    let task_id: Option<String> = match request_task_id {
        Some(t) if !t.is_empty() => Some(t.to_string()),
        _ => {
            // SurrealDB requires ORDER BY fields to appear in the projection.
            let q = format!(
                "SELECT task_id, ord FROM {TBL_A2A_TASK} WHERE context_id = $context_id ORDER BY ord DESC LIMIT 1"
            );
            let mut resp = store
                .db()
                .query(&q)
                .bind(("context_id", context_id.to_string()))
                .await
                .map_err(map_err)?;
            let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
            rows.first().and_then(|r| {
                r.get("task_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
        }
    };

    let Some(ref tid) = task_id else {
        return Ok(ConversationResumeUiHints::default());
    };

    let q = format!("SELECT status_json FROM {TBL_A2A_TASK} WHERE task_id = $task_id LIMIT 1");
    let mut resp = store
        .db()
        .query(&q)
        .bind(("task_id", tid.clone()))
        .await
        .map_err(map_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(map_err)?;
    let status_json = rows
        .first()
        .and_then(|r| r.get("status_json").and_then(|s| s.as_str()))
        .unwrap_or("");

    if status_json.is_empty() {
        return Ok(ConversationResumeUiHints {
            effective_task_id: Some(tid.clone()),
            ..Default::default()
        });
    }

    let Ok(v) = serde_json::from_str::<Value>(status_json) else {
        return Ok(ConversationResumeUiHints {
            effective_task_id: Some(tid.clone()),
            ..Default::default()
        });
    };

    let state_str = v.get("state").and_then(task_state_display);
    let awaiting_input = state_str.as_deref() == Some(INPUT_REQUIRED);
    let input_required_prompt = if awaiting_input {
        v.get("message").and_then(prompt_from_status_message)
    } else {
        None
    };

    Ok(ConversationResumeUiHints {
        effective_task_id: Some(tid.clone()),
        awaiting_input,
        input_required_prompt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_required_and_prompt_from_status_json() {
        let raw =
            r#"{"state":"TASK_STATE_INPUT_REQUIRED","message":{"parts":[{"text":"Pick one."}]}}"#;
        let v: Value = serde_json::from_str(raw).unwrap();
        let state_str = v.get("state").and_then(task_state_display);
        assert_eq!(state_str.as_deref(), Some(INPUT_REQUIRED));
        let p = v.get("message").and_then(prompt_from_status_message);
        assert_eq!(p.as_deref(), Some("Pick one."));
    }
}
