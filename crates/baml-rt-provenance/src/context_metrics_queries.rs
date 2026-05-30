// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Context-level token usage analytics via SurrealDB.
//!
//! Returns typed rows as `Vec<HashMap<String, Value>>` for compatibility
//! with existing consumer code in baml-agent-runner.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    error::Result,
    id_semantics::{context_entity_id_string, task_execution_activity_id_string},
    surreal_store::{SurrealProvenanceStore, check_and_take_zero, map_surreal_error},
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{a2a_relations, context_scope, semantic_labels, storage_safe},
};

pub type MetricsRow = HashMap<String, Value>;

pub async fn session_totals_by_context(
    store: &SurrealProvenanceStore,
    context_id: &str,
) -> Result<Vec<MetricsRow>> {
    let ctx_node = context_entity_id_string(context_id);
    let scoped = context_scope::SCOPED_TO;
    let query = format!(
        "SELECT \
            math::sum(props.a2a_usage_prompt_tokens) AS tokens_in, \
            math::sum(props.a2a_usage_completion_tokens) AS tokens_out, \
            math::sum(props.a2a_usage_total_tokens) AS tokens_total, \
            count() AS llm_call_count, \
            math::sum(props.a2a_duration_ms) AS llm_duration_ms_total \
        FROM {TBL_NODE} \
        WHERE label = 'LlmCall' \
          AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE to_id = $ctx_node AND rel_type = '{scoped}' AND from_label = 'LlmCall') \
          AND props.a2a_usage_total_tokens IS NOT NULL \
        GROUP ALL"
    );
    let response = store
        .db()
        .query(&query)
        .bind(("ctx_node", ctx_node))
        .await
        .map_err(map_surreal_error)?;
    let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| {
            v.as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        })
        .collect())
}

pub async fn turn_totals_by_context(
    store: &SurrealProvenanceStore,
    context_id: &str,
) -> Result<Vec<MetricsRow>> {
    let ctx_node = context_entity_id_string(context_id);
    let scoped = context_scope::SCOPED_TO;
    let was_invoked_by = semantic_labels::WAS_INVOKED_BY;

    // Context-scoped only: both message-processing and LLM nodes must be linked to this context.
    let edge_query = format!(
        "SELECT from_id, to_id OMIT id FROM {TBL_EDGE} \
         WHERE rel_type = '{was_invoked_by}' \
           AND from_label = 'A2AMessageProcessing' \
           AND to_label = 'LlmCall' \
           AND from_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
             WHERE to_id = $ctx_node AND rel_type = '{scoped}') \
           AND to_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
             WHERE to_id = $ctx_node AND rel_type = '{scoped}')"
    );
    let edge_response = store
        .db()
        .query(&edge_query)
        .bind(("ctx_node", ctx_node.clone()))
        .await
        .map_err(map_surreal_error)?;
    let edge_rows: Vec<Value> = check_and_take_zero(edge_response, map_surreal_error)?;

    let msg_ids: Vec<String> = edge_rows
        .iter()
        .filter_map(|r| r.get("from_id").and_then(Value::as_str).map(String::from))
        .collect();
    let llm_ids: Vec<String> = edge_rows
        .iter()
        .filter_map(|r| r.get("to_id").and_then(Value::as_str).map(String::from))
        .collect();

    if msg_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut msg_rows: Vec<Value> = Vec::new();
    for nid in &msg_ids {
        let response = store
            .db()
            .query(format!(
                "SELECT node_id, props.a2a_message_id AS message_id OMIT id \
                 FROM {TBL_NODE} WHERE node_id = $nid LIMIT 1"
            ))
            .bind(("nid", nid.clone()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        msg_rows.extend(rows);
    }
    let msg_map: HashMap<String, String> = msg_rows
        .iter()
        .filter_map(|r| {
            let nid = r.get("node_id").and_then(Value::as_str)?;
            let mid = r.get("message_id").and_then(Value::as_str)?;
            Some((nid.to_string(), mid.to_string()))
        })
        .collect();

    let mut llm_rows: Vec<Value> = Vec::new();
    for nid in &llm_ids {
        let response = store
            .db()
            .query(format!(
                "SELECT node_id, props OMIT id FROM {TBL_NODE} \
                 WHERE node_id = $nid LIMIT 1"
            ))
            .bind(("nid", nid.clone()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        llm_rows.extend(rows);
    }
    let llm_map: HashMap<String, Value> = llm_rows
        .iter()
        .filter_map(|r| {
            let nid = r.get("node_id").and_then(Value::as_str)?;
            Some((
                nid.to_string(),
                r.get("props").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect();

    #[derive(Default)]
    struct TurnAccum {
        tokens_in: i64,
        tokens_out: i64,
        tokens_total: i64,
        llm_call_count: u64,
        llm_duration_ms_total: i64,
        first_activity_anchor: String,
        tail_event_order: u64,
        tail_anchor: String,
        prompt_context_bytes_current: u64,
        prompt_message_chars_current: u64,
    }

    // Join edges with node data to compute per-message aggregates
    let mut by_message: HashMap<String, TurnAccum> = HashMap::new();
    for edge in &edge_rows {
        let from_id = edge
            .get("from_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let to_id = edge
            .get("to_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let message_id = match msg_map.get(from_id) {
            Some(m) => m.clone(),
            None => continue,
        };
        let props = match llm_map.get(to_id) {
            Some(p) => p,
            None => continue,
        };
        let entry = by_message.entry(message_id).or_default();
        entry.tokens_in += props
            .get(storage_safe::A2A_USAGE_PROMPT_TOKENS)
            .and_then(Value::as_i64)
            .unwrap_or(0);
        entry.tokens_out += props
            .get(storage_safe::A2A_USAGE_COMPLETION_TOKENS)
            .and_then(Value::as_i64)
            .unwrap_or(0);
        entry.tokens_total += props
            .get(storage_safe::A2A_USAGE_TOTAL_TOKENS)
            .and_then(Value::as_i64)
            .unwrap_or(0);
        entry.llm_call_count += 1;
        entry.llm_duration_ms_total += props
            .get(storage_safe::A2A_DURATION_MS)
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let anchor = props
            .get(storage_safe::A2A_ACTIVITY_ANCHOR)
            .and_then(Value::as_str)
            .unwrap_or_default();
        if entry.first_activity_anchor.is_empty() || anchor < entry.first_activity_anchor.as_str() {
            entry.first_activity_anchor = anchor.to_string();
        }
        let eo = props
            .get(storage_safe::A2A_EVENT_ORDER)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let pbytes = props
            .get(storage_safe::A2A_PROMPT_SERIALIZED_UTF8_BYTES)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let pchars = props
            .get(storage_safe::A2A_PROMPT_MESSAGE_CHARS)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if eo > entry.tail_event_order
            || (eo == entry.tail_event_order && anchor > entry.tail_anchor.as_str())
        {
            entry.tail_event_order = eo;
            entry.tail_anchor = anchor.to_string();
            entry.prompt_context_bytes_current = pbytes;
            entry.prompt_message_chars_current = pchars;
        }
    }

    let mut results: Vec<(String, MetricsRow)> = by_message
        .into_iter()
        .map(|(mid, acc)| {
            let mut row = MetricsRow::new();
            row.insert("message_id".into(), Value::String(mid.clone()));
            row.insert("tokens_in".into(), serde_json::json!(acc.tokens_in));
            row.insert("tokens_out".into(), serde_json::json!(acc.tokens_out));
            row.insert("tokens_total".into(), serde_json::json!(acc.tokens_total));
            row.insert(
                "llm_call_count".into(),
                serde_json::json!(acc.llm_call_count),
            );
            row.insert(
                "llm_duration_ms_total".into(),
                serde_json::json!(acc.llm_duration_ms_total),
            );
            row.insert(
                "first_activity_anchor".into(),
                Value::String(acc.first_activity_anchor.clone()),
            );
            row.insert(
                "prompt_context_bytes_current".into(),
                serde_json::json!(acc.prompt_context_bytes_current),
            );
            row.insert(
                "prompt_message_chars_current".into(),
                serde_json::json!(acc.prompt_message_chars_current),
            );
            (acc.first_activity_anchor.clone(), row)
        })
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(results.into_iter().map(|(_, row)| row).collect())
}

pub async fn user_prompts_by_context(
    store: &SurrealProvenanceStore,
    context_id: &str,
) -> Result<Vec<MetricsRow>> {
    let ctx_node = context_entity_id_string(context_id);
    let scoped = context_scope::SCOPED_TO;
    let query = format!(
        "SELECT props.a2a_message_id AS message_id, count() AS user_prompt_count \
        FROM {TBL_NODE} \
        WHERE label = 'Message' \
          AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE to_id = $ctx_node AND rel_type = '{scoped}' AND from_label = 'Message') \
          AND props.a2a_direction = 'received' \
          AND (props.a2a_role = 'user' OR props.a2a_role = 'ROLE_USER') \
        GROUP BY props.a2a_message_id"
    );
    let response = store
        .db()
        .query(&query)
        .bind(("ctx_node", ctx_node))
        .await
        .map_err(map_surreal_error)?;
    let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| {
            v.as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        })
        .collect())
}

/// Latest completed `LlmCall` in the context (optional task filter): temporal tail for prompt metrics.
pub async fn session_prompt_context_tail(
    store: &SurrealProvenanceStore,
    context_id: &str,
    task_id: Option<&str>,
) -> Result<Option<MetricsRow>> {
    let ctx_node = context_entity_id_string(context_id);
    let scoped = context_scope::SCOPED_TO;
    let tc = a2a_relations::TASK_CALL;

    let task_clause = match task_id {
        None => String::new(),
        Some(_) => format!(
            " AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id = $task_exec_id AND rel_type = '{tc}')"
        ),
    };

    let a_anchor = storage_safe::A2A_ACTIVITY_ANCHOR;
    let a_eo = storage_safe::A2A_EVENT_ORDER;
    let a_bytes = storage_safe::A2A_PROMPT_SERIALIZED_UTF8_BYTES;
    let a_chars = storage_safe::A2A_PROMPT_MESSAGE_CHARS;

    let query = format!(
        "SELECT \
           props.{a_anchor} AS activity_anchor, \
           props.{a_eo} AS event_order, \
           props.{a_bytes} AS prompt_context_bytes_current, \
           props.{a_chars} AS prompt_message_chars_current \
         FROM {TBL_NODE} \
         WHERE label = 'LlmCall' \
           AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
             WHERE to_id = $ctx_node AND rel_type = '{scoped}' AND from_label = 'LlmCall') \
         {task_clause} \
         ORDER BY event_order DESC, activity_anchor DESC \
         LIMIT 1",
    );

    let mut q = store.db().query(&query).bind(("ctx_node", ctx_node));
    if let Some(tid) = task_id {
        q = q.bind(("task_exec_id", task_execution_activity_id_string(tid)));
    }

    let response = q.await.map_err(map_surreal_error)?;
    let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
    Ok(rows.into_iter().next().and_then(|v| {
        v.as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }))
}

/// Ordered LLM prompt operations through `max_event_order`, optionally after an exclusive lower bound.
pub async fn llm_prompt_operations_for_context(
    store: &SurrealProvenanceStore,
    context_id: &str,
    task_id: Option<&str>,
    max_event_order: u64,
    after_event_order_exclusive: Option<u64>,
) -> Result<Vec<MetricsRow>> {
    let ctx_node = context_entity_id_string(context_id);
    let scoped = context_scope::SCOPED_TO;
    let tc = a2a_relations::TASK_CALL;

    let task_clause = match task_id {
        None => String::new(),
        Some(_) => format!(
            " AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
               WHERE from_id = $task_exec_id AND rel_type = '{tc}')"
        ),
    };

    let a_eo = storage_safe::A2A_EVENT_ORDER;
    let after_clause = match after_event_order_exclusive {
        None => String::new(),
        Some(_) => format!(" AND props.{a_eo} > $after_eo"),
    };

    let a_anchor = storage_safe::A2A_ACTIVITY_ANCHOR;
    let a_bytes = storage_safe::A2A_PROMPT_SERIALIZED_UTF8_BYTES;
    let a_chars = storage_safe::A2A_PROMPT_MESSAGE_CHARS;

    let query = format!(
        "SELECT \
           props.{a_anchor} AS activity_anchor, \
           props.{a_eo} AS event_order, \
           props.{a_bytes} AS prompt_context_bytes_current, \
           props.{a_chars} AS prompt_message_chars_current \
         FROM {TBL_NODE} \
         WHERE label = 'LlmCall' \
           AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
             WHERE to_id = $ctx_node AND rel_type = '{scoped}' AND from_label = 'LlmCall') \
           AND props.{a_eo} <= $max_eo \
         {task_clause} \
         {after_clause} \
         ORDER BY event_order ASC, activity_anchor ASC",
    );

    let mut q = store
        .db()
        .query(&query)
        .bind(("ctx_node", ctx_node))
        .bind(("max_eo", max_event_order));
    if let Some(after) = after_event_order_exclusive {
        q = q.bind(("after_eo", after));
    }
    if let Some(tid) = task_id {
        q = q.bind(("task_exec_id", task_execution_activity_id_string(tid)));
    }

    let response = q.await.map_err(map_surreal_error)?;
    let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| {
            v.as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        })
        .collect())
}
