//! Ops projection helpers — align summary counts with episode TASK_CALL semantics.

use serde_json::Value;

use crate::store::ProvenanceOpsQueryResponse;

/// Override LLM summary `count` with task-scoped TASK_CALL aggregate (matches episode).
pub fn project_ops_llm_summary_count(response: &mut ProvenanceOpsQueryResponse, count: u32) {
    if let Some(obj) = response.summary.as_object_mut() {
        obj.insert("count".to_string(), Value::from(count));
    }
}
