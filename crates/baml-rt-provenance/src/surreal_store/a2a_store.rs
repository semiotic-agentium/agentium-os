//! A2A task subgraph persistence ([`A2aGraphStore`]).

use async_trait::async_trait;
use baml_rt_vocabulary::{
    A2aGraphStore, A2aGraphStoreError, A2aGraphStoreResult, TaskSubgraphNode,
    TaskSubgraphUpdateNode,
};
use serde_json::Value;

use super::{SurrealProvenanceStore, helpers::query_take_zero};
use crate::surreal_tables::{TBL_A2A_MESSAGE, TBL_A2A_TASK, TBL_A2A_UPDATE};

#[async_trait]
impl A2aGraphStore for SurrealProvenanceStore {
    async fn max_task_ord(&self) -> A2aGraphStoreResult<i64> {
        let query = format!("SELECT ord FROM {TBL_A2A_TASK} ORDER BY ord DESC LIMIT 1");
        let rows: Vec<Value> = self
            .query_sql_rows_mapped(&query, A2aGraphStoreError::backend)
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("ord").and_then(Value::as_i64))
            .unwrap_or(0))
    }

    async fn max_message_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64> {
        let query = format!(
            "SELECT seq FROM {TBL_A2A_MESSAGE} WHERE task_id = $task_id ORDER BY seq DESC LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
        Ok(rows
            .first()
            .and_then(|r| r.get("seq").and_then(Value::as_i64))
            .unwrap_or(0))
    }

    async fn max_update_seq(&self, task_id: &str) -> A2aGraphStoreResult<i64> {
        let query = format!(
            "SELECT seq FROM {TBL_A2A_UPDATE} WHERE task_id = $task_id ORDER BY seq DESC LIMIT 1"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
        Ok(rows
            .first()
            .and_then(|r| r.get("seq").and_then(Value::as_i64))
            .unwrap_or(0))
    }

    async fn get_task_node(&self, id: &str) -> A2aGraphStoreResult<Option<TaskSubgraphNode>> {
        let query =
            format!("SELECT * OMIT id FROM {TBL_A2A_TASK} WHERE task_id = $task_id LIMIT 1");
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
        Ok(rows.first().and_then(|row| {
            Some(TaskSubgraphNode {
                id: row.get("task_id")?.as_str()?.to_string(),
                context_id: row
                    .get("context_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status_json: row
                    .get("status_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                metadata_json: row
                    .get("metadata_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                extra_json: row
                    .get("extra_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                artifacts_json: row
                    .get("artifacts_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        }))
    }

    async fn list_task_nodes(
        &self,
        context_id: Option<&str>,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphNode>> {
        let (query, _needs_bind) = if context_id.is_some() {
            (
                format!(
                    "SELECT * OMIT id FROM {TBL_A2A_TASK} WHERE context_id = $context_id ORDER BY ord"
                ),
                true,
            )
        } else {
            (
                format!("SELECT * OMIT id FROM {TBL_A2A_TASK} ORDER BY ord"),
                false,
            )
        };
        let mut q = self.db.query(&query);
        if let Some(cid) = context_id {
            q = q.bind(("context_id", cid.to_string()));
        }
        let mut response = q.await.map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(TaskSubgraphNode {
                    id: row.get("task_id")?.as_str()?.to_string(),
                    context_id: row
                        .get("context_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    status_json: row
                        .get("status_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    metadata_json: row
                        .get("metadata_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    extra_json: row
                        .get("extra_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    artifacts_json: row
                        .get("artifacts_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect())
    }

    async fn upsert_task_node(
        &self,
        node: &TaskSubgraphNode,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        // ord is only set on create (ON CREATE SET semantics).
        // On update, all other fields are overwritten but ord is preserved.
        let query = format!(
            "UPSERT {TBL_A2A_TASK} SET task_id = $task_id, context_id = $context_id, \
             status_json = $status_json, metadata_json = $metadata_json, \
             extra_json = $extra_json, artifacts_json = $artifacts_json, \
             ord = IF ord IS NONE THEN $ord ELSE ord END \
             WHERE task_id = $task_id"
        );
        self.db
            .query(&query)
            .bind(("task_id", node.id.clone()))
            .bind(("context_id", node.context_id.clone()))
            .bind(("status_json", node.status_json.clone()))
            .bind(("metadata_json", node.metadata_json.clone()))
            .bind(("extra_json", node.extra_json.clone()))
            .bind(("artifacts_json", node.artifacts_json.clone()))
            .bind(("ord", ord_if_create))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }

    async fn ensure_task_node(
        &self,
        id: &str,
        context_id: &str,
        ord_if_create: i64,
    ) -> A2aGraphStoreResult<()> {
        // Atomic: UPSERT creates if no match, does nothing meaningful on match
        // (all fields are idempotently re-set to their current values by the WHERE match).
        // On create, the defaults mirror ON CREATE SET semantics.
        let query = format!(
            "UPSERT {TBL_A2A_TASK} SET task_id = $task_id, \
             context_id = IF context_id IS NONE THEN $context_id ELSE context_id END, \
             status_json = IF status_json IS NONE THEN '' ELSE status_json END, \
             metadata_json = IF metadata_json IS NONE THEN '{{}}' ELSE metadata_json END, \
             extra_json = IF extra_json IS NONE THEN '{{}}' ELSE extra_json END, \
             artifacts_json = IF artifacts_json IS NONE THEN '[]' ELSE artifacts_json END, \
             ord = IF ord IS NONE THEN $ord ELSE ord END \
             WHERE task_id = $task_id"
        );
        self.db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .bind(("context_id", context_id.to_string()))
            .bind(("ord", ord_if_create))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }

    async fn insert_message_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        message_json: &str,
    ) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPSERT {TBL_A2A_MESSAGE} SET msg_id = $msg_id, task_id = $task_id, seq = $seq, message_json = $message_json WHERE msg_id = $msg_id"
        );
        self.db
            .query(&query)
            .bind(("msg_id", id.to_string()))
            .bind(("task_id", task_id.to_string()))
            .bind(("seq", seq))
            .bind(("message_json", message_json.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }

    async fn list_message_json(&self, task_id: &str) -> A2aGraphStoreResult<Vec<String>> {
        let query = format!(
            "SELECT message_json, seq FROM {TBL_A2A_MESSAGE} WHERE task_id = $task_id ORDER BY seq"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                row.get("message_json")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect())
    }

    async fn set_task_status_json(&self, id: &str, status_json: &str) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPDATE {TBL_A2A_TASK} SET status_json = $status_json WHERE task_id = $task_id"
        );
        self.db
            .query(&query)
            .bind(("task_id", id.to_string()))
            .bind(("status_json", status_json.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }

    async fn insert_update_node(
        &self,
        id: &str,
        task_id: &str,
        seq: i64,
        kind: &str,
        payload_json: &str,
    ) -> A2aGraphStoreResult<()> {
        let query = format!(
            "UPSERT {TBL_A2A_UPDATE} SET update_id = $update_id, task_id = $task_id, seq = $seq, kind = $kind, payload_json = $payload_json WHERE update_id = $update_id"
        );
        self.db
            .query(&query)
            .bind(("update_id", id.to_string()))
            .bind(("task_id", task_id.to_string()))
            .bind(("seq", seq))
            .bind(("kind", kind.to_string()))
            .bind(("payload_json", payload_json.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }

    async fn list_update_nodes(
        &self,
        task_id: &str,
    ) -> A2aGraphStoreResult<Vec<TaskSubgraphUpdateNode>> {
        let query = format!(
            "SELECT update_id, kind, payload_json, seq FROM {TBL_A2A_UPDATE} WHERE task_id = $task_id ORDER BY seq"
        );
        let mut response = self
            .db
            .query(&query)
            .bind(("task_id", task_id.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        let rows: Vec<Value> = query_take_zero(&mut response, A2aGraphStoreError::backend)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(TaskSubgraphUpdateNode {
                    id: row.get("update_id")?.as_str()?.to_string(),
                    kind: row.get("kind")?.as_str()?.to_string(),
                    payload_json: row.get("payload_json")?.as_str()?.to_string(),
                })
            })
            .collect())
    }

    async fn delete_update_node(&self, id: &str) -> A2aGraphStoreResult<()> {
        let query = format!("DELETE FROM {TBL_A2A_UPDATE} WHERE update_id = $update_id");
        self.db
            .query(&query)
            .bind(("update_id", id.to_string()))
            .await
            .map_err(A2aGraphStoreError::backend)?;
        Ok(())
    }
}
