//! [`TranscriptReader`] — index-backed bounded transcript slices.

use async_trait::async_trait;
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::Result,
    id_semantics::task_entity_id_string_raw,
    read::{TranscriptReader, TranscriptSlice, TranscriptSliceSpec},
    surreal_tables::{TBL_CONTEXT_TRANSCRIPT_INDEX, TBL_NODE},
};

impl SurrealProvenanceStore {
    async fn fetch_transcript_index_rows(
        &self,
        context_id: &str,
        after_event_order: u64,
        limit: usize,
        task_entity_id: Option<&str>,
    ) -> Result<Vec<Value>> {
        let task_filter = if task_entity_id.is_some() {
            "AND task_entity_id = $task_entity_id"
        } else {
            ""
        };
        let query = format!(
            "SELECT node_id, label, event_order FROM {TBL_CONTEXT_TRANSCRIPT_INDEX} \
             WHERE context_id = $context_id AND event_order > $after {task_filter} \
             ORDER BY event_order ASC, node_id ASC LIMIT $limit"
        );
        let mut q = self
            .db
            .query(&query)
            .bind(("context_id", context_id.to_string()))
            .bind(("after", after_event_order))
            .bind(("limit", limit));
        if let Some(task_entity) = task_entity_id {
            q = q.bind(("task_entity_id", task_entity.to_string()));
        }
        let response = q.await.map_err(map_surreal_error)?;
        check_and_take_zero(response, map_surreal_error)
    }

    async fn fetch_transcript_nodes_by_ids(&self, node_ids: &[String]) -> Result<Vec<Value>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let query = format!(
            "SELECT node_id, label, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN $ids \
             AND (label != 'ToolCall' OR props.a2a_activity_outcome IN ['Success', 'Failed']) \
             ORDER BY event_order ASC, node_id ASC"
        );
        let response = self
            .db
            .query(&query)
            .bind(("ids", node_ids.to_vec()))
            .await
            .map_err(map_surreal_error)?;
        check_and_take_zero(response, map_surreal_error)
    }
}

#[async_trait]
impl TranscriptReader for SurrealProvenanceStore {
    async fn slice(&self, spec: TranscriptSliceSpec) -> Result<TranscriptSlice> {
        let limit = spec.limit.max(1);
        let ctx = spec.context_id.as_str();
        let task_entity = spec
            .task_id
            .as_ref()
            .map(|t| task_entity_id_string_raw(t.as_str()));

        let index_rows = self
            .fetch_transcript_index_rows(ctx, spec.after_event_order, limit, task_entity.as_deref())
            .await?;

        if index_rows.is_empty() {
            return Ok(TranscriptSlice {
                items: Vec::new(),
                max_event_order: spec.after_event_order,
                next_after_event_order: None,
            });
        }

        let node_ids: Vec<String> = index_rows
            .iter()
            .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(str::to_string))
            .collect();

        let rows = self.fetch_transcript_nodes_by_ids(&node_ids).await?;

        let items = self
            .conversation_context_filtered(
                &spec.context_id,
                None,
                spec.task_id.as_ref(),
                spec.agent_package.as_deref(),
                Some(spec.after_event_order),
                true,
                spec.include_extensions,
                Some(rows),
            )
            .await?;

        let max_event_order = items
            .iter()
            .map(|i| i.timestamp_ms)
            .max()
            .unwrap_or(spec.after_event_order);
        let next_after_event_order = if index_rows.len() >= limit {
            Some(max_event_order)
        } else {
            None
        };

        Ok(TranscriptSlice {
            items,
            max_event_order,
            next_after_event_order,
        })
    }
}
