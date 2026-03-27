//! Citation query API for provenance.
//!
//! SurrealQL notes:
//! - `props.*` fields are `NONE` (not `NULL`) when absent — always coalesce before operations.
//! - `citations` may be stored as a JSON-string array (`'["#1"]'`) or a native SurrealDB array
//!   depending on the write path. `array::len` errors on string values, so we filter with
//!   `props.citations IS NOT NONE` and do emptiness checks in Rust instead.
//!
//! Queries citations recorded on LLM activity nodes and plan step entities.
//! Citations are stored as a `citations` attribute (raw strings like `"#1"`, `"@4:2"`)
//! on:
//! - LLM call activities (`label = 'LlmCall'`)
//! - Plan step entities (`label = 'PlanStep'`)
//!
//! Full `WAS_DERIVED_FROM` edge creation is Phase 3 work (requires resolving
//! ref numbers to activity anchors via RefTable at call time, before the emission is recorded).

use baml_rt_core::Citation;
use serde_json::Value;

use crate::{
    error::{ProvenanceError, Result},
    surreal_store::SurrealProvenanceStore,
    surreal_tables::{TBL_EDGE, TBL_NODE},
};

fn surreal_err(e: surrealdb::Error) -> ProvenanceError {
    ProvenanceError::Storage(Box::new(e))
}

/// Citation entry returned by query functions.
#[derive(Debug, Clone)]
pub struct CitationEntry {
    /// Activity or entity ID this citation was recorded on.
    pub node_id: String,
    /// Parsed ref-table citations (invalid stored strings are skipped).
    pub citations: Vec<Citation>,
    /// Task ID, if available.
    pub task_id: Option<String>,
    /// Function name, if available (LLM activities only).
    pub function_name: Option<String>,
    /// Plan-local step id when this row is a `PlanStep` node.
    pub step_id: Option<String>,
}

/// Retrieve citations recorded on all LLM activities within a task.
///
/// Returns one `CitationEntry` per activity that has a non-empty `citations` array.
pub async fn query_step_citations(
    store: &SurrealProvenanceStore,
    task_id: &str,
) -> Result<Vec<CitationEntry>> {
    let task_exec = crate::id_semantics::task_execution_activity_id_string(task_id);
    let task_call = crate::vocabulary::a2a_relations::TASK_CALL;
    let query = format!(
        "SELECT id, props.citations AS citations, \
               props.a2a_task_id AS task_id, \
               props.a2a_function_name AS function_name \
        FROM {TBL_NODE} \
        WHERE label = 'LlmCall' \
          AND node_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
            WHERE from_id = $task_exec AND rel_type = '{task_call}' AND to_label = 'LlmCall') \
          AND props.citations IS NOT NONE"
    );
    let mut resp = store
        .db()
        .query(&query)
        .bind(("task_exec", task_exec))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| parse_citation_row(&v))
        // Rust-side non-empty guard (handles both array and JSON-string citations)
        .filter(|e| !e.citations.is_empty())
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
    let plan_node_id = crate::id_semantics::plan_entity_id_string_raw(task_id, plan_id);
    let derived = crate::vocabulary::prov_relations::WAS_DERIVED_FROM;
    let query = format!(
        "SELECT id, props.citations AS citations, \
               props.a2a_task_id AS task_id, \
               props.a2a_step_id AS step_id \
        FROM {TBL_NODE} \
        WHERE label = 'PlanStep' \
          AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE to_id = $plan_node AND rel_type = '{derived}' AND from_label = 'PlanStep') \
          AND props.citations IS NOT NONE"
    );
    let mut resp = store
        .db()
        .query(&query)
        .bind(("plan_node", plan_node_id))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|v| parse_citation_row(&v))
        .filter(|e| !e.citations.is_empty())
        .collect())
}

/// Coverage analysis: find plan steps within a task that have no citations.
///
/// Returns one `CitationEntry` (with empty `citations` vec) per uncited step.
pub async fn query_uncited_steps(
    store: &SurrealProvenanceStore,
    task_id: &str,
) -> Result<Vec<CitationEntry>> {
    let task_node = crate::id_semantics::task_entity_id_string_raw(task_id);
    let has_plan = crate::vocabulary::semantic_labels::HAS_PLAN;
    let derived = crate::vocabulary::prov_relations::WAS_DERIVED_FROM;
    let query = format!(
        "SELECT id, props.a2a_task_id AS task_id, \
               props.a2a_step_id AS step_id \
        FROM {TBL_NODE} \
        WHERE label = 'PlanStep' \
          AND node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
            WHERE to_id IN (SELECT VALUE to_id FROM {TBL_EDGE} \
              WHERE from_id = $task_node AND rel_type = '{has_plan}') \
            AND rel_type = '{derived}' AND from_label = 'PlanStep') \
          AND (props.citations IS NONE OR (type::is::array(props.citations) AND array::len(props.citations) = 0))"
    );
    let mut resp = store
        .db()
        .query(&query)
        .bind(("task_node", task_node))
        .await
        .map_err(surreal_err)?;
    let rows: Vec<Value> = resp.take(0).map_err(surreal_err)?;
    Ok(rows
        .into_iter()
        .map(|v| CitationEntry {
            node_id: v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            citations: vec![],
            task_id: v.get("task_id").and_then(Value::as_str).map(str::to_string),
            function_name: None,
            step_id: v.get("step_id").and_then(Value::as_str).map(str::to_string),
        })
        .collect())
}

fn parse_citation_row(v: &Value) -> Option<CitationEntry> {
    let citations: Vec<Citation> = v
        .get("citations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().and_then(|t| Citation::try_new(t).ok()))
                .collect()
        })
        .unwrap_or_default();
    Some(CitationEntry {
        node_id: v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        citations,
        task_id: v.get("task_id").and_then(Value::as_str).map(str::to_string),
        function_name: v
            .get("function_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        step_id: v.get("step_id").and_then(Value::as_str).map(str::to_string),
    })
}
