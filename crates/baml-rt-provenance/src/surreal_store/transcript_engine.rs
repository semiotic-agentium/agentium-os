//! [`TranscriptEngine`] — index-backed bounded transcript pages.

use async_trait::async_trait;
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::Result,
    id_semantics::task_entity_id_string_raw,
    read::{TranscriptEngine, TranscriptPage, TranscriptPageRequest, TranscriptScopeWidening},
    surreal_tables::{TBL_CONTEXT_TRANSCRIPT_INDEX, TBL_NODE},
};

impl SurrealProvenanceStore {
    async fn fetch_transcript_index_rows(
        &self,
        context_id: &str,
        after_exclusive: Option<u64>,
        limit: usize,
        task_entity_id: Option<&str>,
    ) -> Result<Vec<Value>> {
        let task_filter = if task_entity_id.is_some() {
            "AND task_entity_id = $task_entity_id"
        } else {
            ""
        };
        let after_filter = if after_exclusive.is_some() {
            "AND event_order > $after"
        } else {
            ""
        };
        let query = format!(
            "SELECT node_id, label, event_order FROM {TBL_CONTEXT_TRANSCRIPT_INDEX} \
             WHERE context_id = $context_id {after_filter} {task_filter} \
             ORDER BY event_order ASC, node_id ASC LIMIT $limit"
        );
        let mut q = self
            .db
            .query(&query)
            .bind(("context_id", context_id.to_string()))
            .bind(("limit", limit));
        if let Some(after) = after_exclusive {
            q = q.bind(("after", after));
        }
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
impl TranscriptEngine for SurrealProvenanceStore {
    async fn page(&self, request: TranscriptPageRequest) -> Result<TranscriptPage> {
        let limit = request.limit.max(1);
        let ctx = request.scope.context_id.as_str();
        let after_exclusive = request.after_event_order_exclusive();
        let task_entity = request
            .scope
            .task_id()
            .map(|t| task_entity_id_string_raw(t.as_str()));

        let mut scope_widening = TranscriptScopeWidening::None;
        let mut index_rows = self
            .fetch_transcript_index_rows(ctx, after_exclusive, limit, task_entity.as_deref())
            .await?;

        if index_rows.is_empty() && task_entity.is_some() {
            scope_widening = TranscriptScopeWidening::ContextFallback;
            index_rows = self
                .fetch_transcript_index_rows(ctx, after_exclusive, limit, None)
                .await?;
        }

        if index_rows.is_empty() {
            let enrich = request.profile.enrich_from_graph_extensions();
            let items = self
                .conversation_context_filtered(
                    &request.scope.context_id,
                    Some(limit),
                    request.scope.task_id(),
                    request.scope.agent_package.as_deref(),
                    after_exclusive,
                    true,
                    enrich,
                    None,
                )
                .await?;
            let max_event_order = items
                .iter()
                .map(|i| i.timestamp_ms)
                .max()
                .unwrap_or(after_exclusive.unwrap_or(0));
            let next_after_event_order = if items.len() >= limit {
                Some(max_event_order)
            } else {
                None
            };
            return Ok(TranscriptPage {
                scope: request.scope,
                items,
                max_event_order,
                next_after_event_order,
                scope_widening,
            });
        }

        let node_ids: Vec<String> = index_rows
            .iter()
            .filter_map(|r| r.get("node_id").and_then(Value::as_str).map(str::to_string))
            .collect();

        let rows = self.fetch_transcript_nodes_by_ids(&node_ids).await?;

        let enrich = request.profile.enrich_from_graph_extensions();
        let items = self
            .conversation_context_filtered(
                &request.scope.context_id,
                None,
                request.scope.task_id(),
                request.scope.agent_package.as_deref(),
                after_exclusive,
                true,
                enrich,
                Some(rows),
            )
            .await?;

        let max_event_order = items
            .iter()
            .map(|i| i.timestamp_ms)
            .max()
            .unwrap_or(after_exclusive.unwrap_or(0));
        let next_after_event_order = if index_rows.len() >= limit {
            Some(max_event_order)
        } else {
            None
        };

        Ok(TranscriptPage {
            scope: request.scope,
            items,
            max_event_order,
            next_after_event_order,
            scope_widening,
        })
    }
}
