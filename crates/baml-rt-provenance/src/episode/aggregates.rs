//! Surreal-backed aggregates for episode projection (task-scoped LLM usage and timing hints).

use baml_rt_vocabulary::vocabulary::{a2a_relations, node_labels, storage_safe};
use serde_json::Value;

use super::TokenSummary;
use crate::{
    error::{ProvenanceError, Result},
    id_semantics::task_execution_activity_id_string,
    surreal_store::SurrealProvenanceStore,
    surreal_tables::{TBL_EDGE, TBL_NODE},
};

fn surreal_err(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

fn json_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().map(|i| i.max(0) as u64))
        .or_else(|| v.as_f64().map(|f| f as u64))
}

/// Sum token usage and LLM duration for all [`node_labels::LLM_CALL`] nodes linked via TASK_CALL.
pub(crate) async fn token_summary_for_task(
    store: &SurrealProvenanceStore,
    task_id: &str,
) -> Result<TokenSummary> {
    let task_exec = task_execution_activity_id_string(task_id);
    let task_call = a2a_relations::TASK_CALL;
    let query = format!(
        "SELECT \
            math::sum(props.{p} ?? 0) AS prompt_tokens, \
            math::sum(props.{c} ?? 0) AS completion_tokens, \
            math::sum(props.{t} ?? 0) AS total_tokens, \
            count() AS llm_call_count, \
            math::sum(props.{d} ?? 0) AS llm_duration_ms \
        FROM {TBL_NODE} \
        WHERE label = '{lbl}' \
          AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
            WHERE from_id = $task_exec AND rel_type = '{task_call}' AND to_label = '{lbl}') \
        GROUP ALL",
        p = storage_safe::A2A_USAGE_PROMPT_TOKENS,
        c = storage_safe::A2A_USAGE_COMPLETION_TOKENS,
        t = storage_safe::A2A_USAGE_TOTAL_TOKENS,
        d = storage_safe::A2A_DURATION_MS,
        lbl = node_labels::LLM_CALL,
    );
    let mut resp = store
        .db()
        .query(&query)
        .bind(("task_exec", task_exec))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    let Some(row) = rows.first().and_then(|v| v.as_object()) else {
        return Ok(TokenSummary::default());
    };
    Ok(TokenSummary {
        prompt_tokens: row.get("prompt_tokens").and_then(json_u64).unwrap_or(0),
        completion_tokens: row.get("completion_tokens").and_then(json_u64).unwrap_or(0),
        total_tokens: row.get("total_tokens").and_then(json_u64).unwrap_or(0),
        llm_call_count: row.get("llm_call_count").and_then(json_u64).unwrap_or(0) as u32,
        llm_duration_ms: row.get("llm_duration_ms").and_then(json_u64).unwrap_or(0),
    })
}

/// Earliest non-zero `prov_startTime` among task LLM call nodes via a DB-level aggregate.
///
/// Uses `math::min` with coalescing fallbacks to `prov_time` so a single round-trip
/// replaces the previous client-side reduce over all rows.
pub(crate) async fn llm_earliest_timestamp_ms(
    store: &SurrealProvenanceStore,
    task_id: &str,
) -> Result<Option<u64>> {
    let task_exec = task_execution_activity_id_string(task_id);
    let task_call = a2a_relations::TASK_CALL;
    let query = format!(
        "SELECT math::min(props.{p} ?? props.{t}) AS earliest \
         FROM {TBL_NODE} \
         WHERE label = '{lbl}' \
           AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
             WHERE from_id = $task_exec AND rel_type = '{task_call}' AND to_label = '{lbl}') \
         GROUP ALL",
        p = storage_safe::PROV_START_TIME,
        t = storage_safe::PROV_TIME,
        lbl = node_labels::LLM_CALL,
    );
    let mut resp = store
        .db()
        .query(&query)
        .bind(("task_exec", task_exec))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .first()
        .and_then(|r| r.get("earliest"))
        .and_then(json_u64)
        .filter(|&t| t > 0))
}
