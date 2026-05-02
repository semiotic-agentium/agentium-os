//! Context-level token usage analytics via SurrealDB.
//!
//! Returns typed rows as `Vec<HashMap<String, Value>>` for compatibility
//! with existing consumer code in baml-agent-runner.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    error::{ProvenanceError, Result},
    id_semantics::context_entity_id_string,
    surreal_store::SurrealProvenanceStore,
    surreal_tables::{TBL_EDGE, TBL_NODE},
    vocabulary::{context_scope, semantic_labels},
};

fn surreal_err(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

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
    let mut resp = store
        .db()
        .query(&query)
        .bind(("ctx_node", ctx_node))
        .await
        .map_err(surreal_err)?
        .check()
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
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
    let mut edge_resp = store
        .db()
        .query(&edge_query)
        .bind(("ctx_node", ctx_node.clone()))
        .await
        .map_err(surreal_err)?
        .check()
        .map_err(surreal_err)?;
    let edge_rows: Vec<Value> = edge_resp.take(0).map_err(surreal_err)?;

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
        let mut resp = store
            .db()
            .query(format!(
                "SELECT node_id, props.a2a_message_id AS message_id OMIT id \
                 FROM {TBL_NODE} WHERE node_id = $nid LIMIT 1"
            ))
            .bind(("nid", nid.clone()))
            .await
            .map_err(surreal_err)?
            .check()
            .map_err(surreal_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
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
        let mut resp = store
            .db()
            .query(format!(
                "SELECT node_id, props OMIT id FROM {TBL_NODE} \
                 WHERE node_id = $nid AND props.a2a_usage_total_tokens IS NOT NULL LIMIT 1"
            ))
            .bind(("nid", nid.clone()))
            .await
            .map_err(surreal_err)?
            .check()
            .map_err(surreal_err)?;
        let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
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

    // Join edges with node data to compute per-message aggregates
    let mut by_message: HashMap<String, (i64, i64, i64, u64, i64, String)> = HashMap::new();
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
        let entry = by_message
            .entry(message_id)
            .or_insert((0, 0, 0, 0, 0, String::new()));
        entry.0 += props
            .get("a2a_usage_prompt_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        entry.1 += props
            .get("a2a_usage_completion_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        entry.2 += props
            .get("a2a_usage_total_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        entry.3 += 1;
        entry.4 += props
            .get("a2a_duration_ms")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let eid = props
            .get("a2a_activity_anchor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if entry.5.is_empty() || eid < entry.5.as_str() {
            entry.5 = eid.to_string();
        }
    }

    let mut results: Vec<(String, MetricsRow)> = by_message
        .into_iter()
        .map(|(mid, (ti, to, tt, cc, dur, feid))| {
            let mut row = MetricsRow::new();
            row.insert("message_id".into(), Value::String(mid.clone()));
            row.insert("tokens_in".into(), serde_json::json!(ti));
            row.insert("tokens_out".into(), serde_json::json!(to));
            row.insert("tokens_total".into(), serde_json::json!(tt));
            row.insert("llm_call_count".into(), serde_json::json!(cc));
            row.insert("llm_duration_ms_total".into(), serde_json::json!(dur));
            row.insert("first_activity_anchor".into(), Value::String(feid.clone()));
            (feid, row)
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
    let mut resp = store
        .db()
        .query(&query)
        .bind(("ctx_node", ctx_node))
        .await
        .map_err(surreal_err)?
        .check()
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| {
            v.as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        })
        .collect())
}
