//! Citation query API for provenance.
//!
//! Queries citations recorded on LLM activity nodes and plan step entities.
//! Citations are stored as a `citations` attribute array (raw strings like `"#1"`, `"@4:2"`)
//! on:
//! - LLM call activities (`label = 'LlmCall'`)
//! - Plan step entities (`label = 'PlanStep'`)
//!
//! Full `WAS_DERIVED_FROM` edge creation is Phase 3 work (requires resolving
//! ref numbers to event_ids via RefTable at call time, before the event is emitted).

use serde_json::Value;

use crate::{
    error::{ProvenanceError, Result},
    surreal_store::SurrealProvenanceStore,
};

fn surreal_err(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

/// Citation entry returned by query functions.
#[derive(Debug, Clone)]
pub struct CitationEntry {
    /// Activity or entity ID this citation was recorded on.
    pub node_id: String,
    /// Raw citation strings (e.g. `["#1", "@4:2"]`).
    pub citations: Vec<String>,
    /// Task ID, if available.
    pub task_id: Option<String>,
    /// Function name, if available (LLM activities only).
    pub function_name: Option<String>,
}

/// Retrieve citations recorded on all LLM activities within a task.
///
/// Returns one `CitationEntry` per activity that has a non-empty `citations` array.
pub async fn query_step_citations(
    store: &SurrealProvenanceStore,
    task_id: &str,
) -> Result<Vec<CitationEntry>> {
    let query = "\
        SELECT id, props.citations AS citations, \
               props.a2a_task_id AS task_id, \
               props.a2a_function_name AS function_name \
        FROM prov_node \
        WHERE label = 'LlmCall' \
          AND props.a2a_task_id = $task_id \
          AND props.citations IS NOT NULL \
          AND array::len(props.citations) > 0";
    let mut resp = store
        .db()
        .query(query)
        .bind(("task_id", task_id.to_string()))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| parse_citation_row(&v))
        .collect())
}

/// Retrieve citations recorded on plan step entities within a task.
///
/// Returns one `CitationEntry` per plan step that has a non-empty `citations` array.
pub async fn query_plan_citations(
    store: &SurrealProvenanceStore,
    task_id: &str,
    plan_id: &str,
) -> Result<Vec<CitationEntry>> {
    let query = "\
        SELECT id, props.citations AS citations, \
               props.a2a_task_id AS task_id \
        FROM prov_node \
        WHERE label = 'PlanStep' \
          AND props.a2a_task_id = $task_id \
          AND props.a2a_plan_id = $plan_id \
          AND props.citations IS NOT NULL \
          AND array::len(props.citations) > 0";
    let mut resp = store
        .db()
        .query(query)
        .bind(("task_id", task_id.to_string()))
        .bind(("plan_id", plan_id.to_string()))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| parse_citation_row(&v))
        .collect())
}

/// Coverage analysis: find plan steps within a task that have no citations.
///
/// Returns one `CitationEntry` (with empty `citations` vec) per uncited step.
pub async fn query_uncited_steps(
    store: &SurrealProvenanceStore,
    task_id: &str,
) -> Result<Vec<CitationEntry>> {
    let query = "\
        SELECT id, props.a2a_task_id AS task_id, \
               props.a2a_step_id AS step_id \
        FROM prov_node \
        WHERE label = 'PlanStep' \
          AND props.a2a_task_id = $task_id \
          AND (props.citations IS NULL OR array::len(props.citations) = 0)";
    let mut resp = store
        .db()
        .query(query)
        .bind(("task_id", task_id.to_string()))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .map(|v| CitationEntry {
            node_id: v.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            citations: vec![],
            task_id: v
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            function_name: None,
        })
        .collect())
}

fn parse_citation_row(v: &Value) -> Option<CitationEntry> {
    let citations: Vec<String> = v
        .get("citations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(CitationEntry {
        node_id: v.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        citations,
        task_id: v
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        function_name: v
            .get("function_name")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}
