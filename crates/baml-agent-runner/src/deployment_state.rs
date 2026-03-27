use std::{collections::HashMap, path::Path, sync::Arc};

use baml_rt_core::{
    BamlRtError, DeploymentContentHash, DeploymentRecord, DeploymentStatus, Result,
};
use baml_tools_system::{
    callback_store::{
        CancelCallbackSelector, ScheduleCallbackRequest, ScheduleCallbackResult, StoredCallback,
    },
    callback_time::callback_now_unix_ms,
};
use serde::Deserialize;
use surrealdb::{
    Surreal,
    engine::local::{Db, Mem, SurrealKv},
};
use tracing::debug;
use uuid::Uuid;

const NS: &str = "baml";
const DB_NAME: &str = "runner_state";
const TBL_DEPLOYMENTS: &str = "deployments";
const TBL_EVENT_PRODUCER_CHECKPOINTS: &str = "event_producer_checkpoints";
const TBL_SCHEDULED_CALLBACKS: &str = "scheduled_callbacks";

const SCHEMA_QUERIES: &[&str] = &[
    "DEFINE TABLE IF NOT EXISTS deployments SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS content_hash ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS agent_name ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS deployed_at ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS status ON deployments TYPE string",
    "DEFINE FIELD IF NOT EXISTS last_error ON deployments TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS last_attempt_at ON deployments TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS failure_count ON deployments TYPE int",
    "DEFINE INDEX IF NOT EXISTS idx_deploy_content_hash ON deployments FIELDS content_hash UNIQUE",
    "DEFINE TABLE IF NOT EXISTS event_producer_checkpoints SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS producer_key ON event_producer_checkpoints TYPE string",
    "DEFINE FIELD IF NOT EXISTS checkpoint ON event_producer_checkpoints TYPE string",
    "DEFINE INDEX IF NOT EXISTS idx_event_producer_checkpoint_key ON event_producer_checkpoints FIELDS producer_key UNIQUE",
    "DEFINE TABLE IF NOT EXISTS scheduled_callbacks SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS callback_id ON scheduled_callbacks TYPE string",
    "DEFINE FIELD IF NOT EXISTS source_key ON scheduled_callbacks TYPE string",
    "DEFINE FIELD IF NOT EXISTS dedupe_key ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS payload_json ON scheduled_callbacks TYPE string",
    "DEFINE FIELD IF NOT EXISTS status ON scheduled_callbacks TYPE string",
    "DEFINE FIELD IF NOT EXISTS scheduled_for_unix_ms ON scheduled_callbacks TYPE int",
    "DEFINE FIELD IF NOT EXISTS requested_at_unix_ms ON scheduled_callbacks TYPE int",
    "DEFINE FIELD IF NOT EXISTS delivered_at_unix_ms ON scheduled_callbacks TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS cancelled_at_unix_ms ON scheduled_callbacks TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS context_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS task_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS requesting_agent_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS requesting_message_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_id ON scheduled_callbacks FIELDS callback_id UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_due ON scheduled_callbacks FIELDS status, scheduled_for_unix_ms",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_dedupe ON scheduled_callbacks FIELDS source_key, dedupe_key, status",
];

pub struct DeploymentStateStore {
    db: Arc<Surreal<Db>>,
}

impl DeploymentStateStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Surreal::new::<SurrealKv>(path.as_ref().to_string_lossy().as_ref())
            .await
            .map_err(to_write_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(to_write_err)?;
        let store = Self { db: Arc::new(db) };
        store.init_schema().await?;
        Ok(store)
    }

    pub async fn open_in_memory() -> Result<Self> {
        let db = Surreal::new::<Mem>(()).await.map_err(to_write_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(to_write_err)?;
        let store = Self { db: Arc::new(db) };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> Result<()> {
        for stmt in SCHEMA_QUERIES {
            self.db.query(*stmt).await.map_err(to_write_err)?;
        }
        Ok(())
    }

    pub async fn save_deployment(&self, record: &DeploymentRecord) -> Result<()> {
        let status_value = serde_json::to_value(&record.status).map_err(|e| {
            BamlRtError::InvalidArgument(format!("failed to serialize deployment status: {e}"))
        })?;
        let status = status_value
            .as_str()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "serialized deployment status is not a string".to_string(),
                )
            })?
            .to_string();

        self.db
            .query(format!(
                "UPSERT {TBL_DEPLOYMENTS} SET \
                    content_hash = $content_hash, \
                    agent_name = $agent_name, \
                    deployed_at = $deployed_at, \
                    status = $status, \
                    last_error = $last_error, \
                    last_attempt_at = $last_attempt_at, \
                    failure_count = $failure_count \
                 WHERE content_hash = $content_hash"
            ))
            .bind(("content_hash", record.content_hash.as_str().to_string()))
            .bind(("agent_name", record.agent_name.clone()))
            .bind(("deployed_at", record.deployed_at.clone()))
            .bind(("status", status))
            .bind(("last_error", record.last_error.clone()))
            .bind(("last_attempt_at", record.last_attempt_at.clone()))
            .bind(("failure_count", record.failure_count as i64))
            .await
            .map_err(to_write_err)?;
        Ok(())
    }

    pub async fn remove_deployment(&self, content_hash: &DeploymentContentHash) -> Result<bool> {
        let mut resp = self
            .db
            .query(format!(
                "DELETE FROM {TBL_DEPLOYMENTS} WHERE content_hash = $content_hash RETURN BEFORE"
            ))
            .bind(("content_hash", content_hash.as_str().to_string()))
            .await
            .map_err(to_write_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        Ok(!rows.is_empty())
    }

    pub async fn list_deployments(&self) -> Result<Vec<DeploymentRecord>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT content_hash,agent_name,deployed_at,status,last_error,last_attempt_at,failure_count FROM {TBL_DEPLOYMENTS}"
            ))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter().map(parse_deployment_row).collect()
    }

    pub async fn save_event_producer_checkpoint(
        &self,
        producer_key: &str,
        checkpoint: &str,
    ) -> Result<()> {
        self.db
            .query(format!(
                "UPSERT {TBL_EVENT_PRODUCER_CHECKPOINTS} SET \
                    producer_key = $producer_key, \
                    checkpoint = $checkpoint \
                 WHERE producer_key = $producer_key"
            ))
            .bind(("producer_key", producer_key.to_string()))
            .bind(("checkpoint", checkpoint.to_string()))
            .await
            .map_err(to_write_err)?;
        Ok(())
    }

    pub async fn list_event_producer_checkpoints(&self) -> Result<HashMap<String, String>> {
        #[derive(Deserialize)]
        struct EventProducerCheckpointRow {
            producer_key: String,
            checkpoint: String,
        }

        let mut resp = self
            .db
            .query(format!(
                "SELECT producer_key,checkpoint FROM {TBL_EVENT_PRODUCER_CHECKPOINTS}"
            ))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter()
            .map(|row| {
                let parsed: EventProducerCheckpointRow =
                    serde_json::from_value(row).map_err(|e| {
                        BamlRtError::InvalidArgument(format!(
                            "invalid event producer checkpoint row from state DB: {e}"
                        ))
                    })?;
                Ok((parsed.producer_key, parsed.checkpoint))
            })
            .collect::<Result<HashMap<_, _>>>()
    }

    pub async fn schedule_callback(
        &self,
        request: &ScheduleCallbackRequest,
    ) -> Result<ScheduleCallbackResult> {
        if let Some(dedupe_key) = &request.dedupe_key
            && let Some(existing) = self
                .find_pending_callback_by_dedupe(&request.source_key, dedupe_key)
                .await?
        {
            return Ok(ScheduleCallbackResult {
                callback: existing,
                created: false,
            });
        }

        let callback = StoredCallback {
            callback_id: Uuid::new_v4().to_string(),
            source_key: request.source_key.clone(),
            dedupe_key: request.dedupe_key.clone(),
            payload: request.payload.clone(),
            scheduled_for_unix_ms: request.scheduled_for_unix_ms,
            requested_at_unix_ms: request.requested_at_unix_ms,
            context_id: request.context_id.clone(),
            task_id: request.task_id.clone(),
            requesting_agent_id: request.requesting_agent_id.clone(),
            requesting_message_id: request.requesting_message_id.clone(),
        };
        self.upsert_callback_row(&callback, "pending", None, None)
            .await?;
        debug!(
            callback_id = %callback.callback_id,
            source_key = %callback.source_key,
            deduped = false,
            scheduled_for_unix_ms = callback.scheduled_for_unix_ms,
            "runner state stored scheduled callback"
        );
        Ok(ScheduleCallbackResult {
            callback,
            created: true,
        })
    }

    pub async fn cancel_callback(
        &self,
        selector: CancelCallbackSelector,
    ) -> Result<Option<StoredCallback>> {
        let callback = match selector {
            CancelCallbackSelector::CallbackId(callback_id) => {
                self.find_pending_callback_by_id(&callback_id).await?
            }
            CancelCallbackSelector::DedupeKey {
                source_key,
                dedupe_key,
            } => {
                self.find_pending_callback_by_dedupe(&source_key, &dedupe_key)
                    .await?
            }
        };

        let Some(callback) = callback else {
            return Ok(None);
        };

        self.upsert_callback_row(
            &callback,
            "cancelled",
            None,
            Some(callback_now_unix_ms("system_callback_cancel")),
        )
        .await?;
        debug!(
            callback_id = %callback.callback_id,
            source_key = %callback.source_key,
            "runner state cancelled callback"
        );
        Ok(Some(callback))
    }

    pub async fn list_due_callbacks(
        &self,
        now_unix_ms: u64,
        limit: usize,
    ) -> Result<Vec<StoredCallback>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT callback_id,source_key,dedupe_key,payload_json,scheduled_for_unix_ms,requested_at_unix_ms,context_id,task_id,requesting_agent_id,requesting_message_id \
                 FROM {TBL_SCHEDULED_CALLBACKS} \
                 WHERE status = 'pending' AND scheduled_for_unix_ms <= $scheduled_for_unix_ms \
                 ORDER BY scheduled_for_unix_ms ASC, callback_id ASC LIMIT $limit"
            ))
            .bind(("scheduled_for_unix_ms", now_unix_ms as i64))
            .bind(("limit", limit as i64))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter().map(parse_callback_row).collect()
    }

    pub async fn mark_callbacks_delivered(
        &self,
        callback_ids: &[String],
        delivered_at_unix_ms: u64,
    ) -> Result<()> {
        if callback_ids.is_empty() {
            return Ok(());
        }

        self.db
            .query(format!(
                "UPDATE {TBL_SCHEDULED_CALLBACKS} SET \
                    status = 'delivered', \
                    delivered_at_unix_ms = $delivered_at_unix_ms, \
                    cancelled_at_unix_ms = NONE \
                 WHERE callback_id INSIDE $callback_ids AND status = 'pending'"
            ))
            .bind(("callback_ids", callback_ids.to_vec()))
            .bind(("delivered_at_unix_ms", delivered_at_unix_ms as i64))
            .await
            .map_err(to_write_err)?;
        debug!(
            delivered_count = callback_ids.len(),
            delivered_at_unix_ms, "runner state marked callbacks delivered"
        );
        Ok(())
    }

    async fn find_pending_callback_by_id(
        &self,
        callback_id: &str,
    ) -> Result<Option<StoredCallback>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT callback_id,source_key,dedupe_key,payload_json,scheduled_for_unix_ms,requested_at_unix_ms,context_id,task_id,requesting_agent_id,requesting_message_id \
                 FROM {TBL_SCHEDULED_CALLBACKS} \
                 WHERE callback_id = $callback_id AND status = 'pending' LIMIT 1"
            ))
            .bind(("callback_id", callback_id.to_string()))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter().next().map(parse_callback_row).transpose()
    }

    async fn find_pending_callback_by_dedupe(
        &self,
        source_key: &str,
        dedupe_key: &str,
    ) -> Result<Option<StoredCallback>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT callback_id,source_key,dedupe_key,payload_json,scheduled_for_unix_ms,requested_at_unix_ms,context_id,task_id,requesting_agent_id,requesting_message_id \
                 FROM {TBL_SCHEDULED_CALLBACKS} \
                 WHERE source_key = $source_key AND dedupe_key = $dedupe_key AND status = 'pending' LIMIT 1"
            ))
            .bind(("source_key", source_key.to_string()))
            .bind(("dedupe_key", dedupe_key.to_string()))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter().next().map(parse_callback_row).transpose()
    }

    async fn upsert_callback_row(
        &self,
        callback: &StoredCallback,
        status: &str,
        delivered_at_unix_ms: Option<u64>,
        cancelled_at_unix_ms: Option<u64>,
    ) -> Result<()> {
        let payload_json = serde_json::to_string(&callback.payload).map_err(|err| {
            BamlRtError::InvalidArgument(format!("failed to serialize callback payload: {err}"))
        })?;
        self.db
            .query(format!(
                "UPSERT {TBL_SCHEDULED_CALLBACKS} SET \
                    callback_id = $callback_id, \
                    source_key = $source_key, \
                    dedupe_key = $dedupe_key, \
                    payload_json = $payload_json, \
                    status = $status, \
                    scheduled_for_unix_ms = $scheduled_for_unix_ms, \
                    requested_at_unix_ms = $requested_at_unix_ms, \
                    delivered_at_unix_ms = $delivered_at_unix_ms, \
                    cancelled_at_unix_ms = $cancelled_at_unix_ms, \
                    context_id = $context_id, \
                    task_id = $task_id, \
                    requesting_agent_id = $requesting_agent_id, \
                    requesting_message_id = $requesting_message_id \
                 WHERE callback_id = $callback_id"
            ))
            .bind(("callback_id", callback.callback_id.clone()))
            .bind(("source_key", callback.source_key.clone()))
            .bind(("dedupe_key", callback.dedupe_key.clone()))
            .bind(("payload_json", payload_json))
            .bind(("status", status.to_string()))
            .bind((
                "scheduled_for_unix_ms",
                callback.scheduled_for_unix_ms as i64,
            ))
            .bind(("requested_at_unix_ms", callback.requested_at_unix_ms as i64))
            .bind((
                "delivered_at_unix_ms",
                delivered_at_unix_ms.map(|value| value as i64),
            ))
            .bind((
                "cancelled_at_unix_ms",
                cancelled_at_unix_ms.map(|value| value as i64),
            ))
            .bind((
                "context_id",
                callback
                    .context_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
            ))
            .bind((
                "task_id",
                callback
                    .task_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
            ))
            .bind(("requesting_agent_id", callback.requesting_agent_id.clone()))
            .bind((
                "requesting_message_id",
                callback
                    .requesting_message_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
            ))
            .await
            .map_err(to_write_err)?;
        Ok(())
    }
}

fn parse_deployment_row(row: serde_json::Value) -> Result<DeploymentRecord> {
    #[derive(Deserialize)]
    struct DeploymentRow {
        content_hash: String,
        agent_name: String,
        deployed_at: String,
        status: DeploymentStatus,
        #[serde(default)]
        last_error: Option<String>,
        #[serde(default)]
        last_attempt_at: Option<String>,
        #[serde(default)]
        failure_count: u32,
    }

    let parsed: DeploymentRow = serde_json::from_value(row).map_err(|e| {
        BamlRtError::InvalidArgument(format!("invalid deployment row from state DB: {e}"))
    })?;

    Ok(DeploymentRecord {
        content_hash: parsed.content_hash.parse().map_err(|e| {
            BamlRtError::InvalidArgument(format!("invalid content_hash in deployment row: {e}"))
        })?,
        agent_name: parsed.agent_name,
        deployed_at: parsed.deployed_at,
        status: parsed.status,
        last_error: parsed.last_error,
        last_attempt_at: parsed.last_attempt_at,
        failure_count: parsed.failure_count,
    })
}

fn parse_callback_row(row: serde_json::Value) -> Result<StoredCallback> {
    #[derive(Deserialize)]
    struct CallbackRow {
        callback_id: String,
        source_key: String,
        #[serde(default)]
        dedupe_key: Option<String>,
        payload_json: String,
        scheduled_for_unix_ms: i64,
        requested_at_unix_ms: i64,
        #[serde(default)]
        context_id: Option<String>,
        #[serde(default)]
        task_id: Option<String>,
        #[serde(default)]
        requesting_agent_id: Option<String>,
        #[serde(default)]
        requesting_message_id: Option<String>,
    }

    let parsed: CallbackRow = serde_json::from_value(row).map_err(|e| {
        BamlRtError::InvalidArgument(format!("invalid callback row from state DB: {e}"))
    })?;
    let payload = serde_json::from_str(&parsed.payload_json).map_err(|e| {
        BamlRtError::InvalidArgument(format!("invalid callback payload_json from state DB: {e}"))
    })?;

    Ok(StoredCallback {
        callback_id: parsed.callback_id,
        source_key: parsed.source_key,
        dedupe_key: parsed.dedupe_key,
        payload,
        scheduled_for_unix_ms: parsed.scheduled_for_unix_ms.max(0) as u64,
        requested_at_unix_ms: parsed.requested_at_unix_ms.max(0) as u64,
        context_id: parsed
            .context_id
            .map(|value| baml_rt_core::ids::ContextId::from(value.as_str())),
        task_id: parsed.task_id.map(|value| {
            baml_rt_core::ids::TaskId::from_external(baml_rt_core::ids::ExternalId::new(value))
        }),
        requesting_agent_id: parsed.requesting_agent_id,
        requesting_message_id: parsed
            .requesting_message_id
            .map(baml_rt_core::ids::MessageId::from),
    })
}

fn to_write_err(err: surrealdb::Error) -> BamlRtError {
    BamlRtError::Io(std::io::Error::other(format!(
        "runner state write failed: {err}"
    )))
}

fn to_read_err(err: surrealdb::Error) -> BamlRtError {
    BamlRtError::Io(std::io::Error::other(format!(
        "runner state read failed: {err}"
    )))
}

#[async_trait::async_trait]
impl baml_tools_system::callback_store::CallbackStore for DeploymentStateStore {
    async fn schedule_callback(
        &self,
        request: ScheduleCallbackRequest,
    ) -> Result<ScheduleCallbackResult> {
        DeploymentStateStore::schedule_callback(self, &request).await
    }

    async fn cancel_callback(
        &self,
        selector: CancelCallbackSelector,
    ) -> Result<Option<StoredCallback>> {
        DeploymentStateStore::cancel_callback(self, selector).await
    }

    async fn list_due_callbacks(
        &self,
        now_unix_ms: u64,
        limit: usize,
    ) -> Result<Vec<StoredCallback>> {
        DeploymentStateStore::list_due_callbacks(self, now_unix_ms, limit).await
    }

    async fn mark_callbacks_delivered(
        &self,
        callback_ids: &[String],
        delivered_at_unix_ms: u64,
    ) -> Result<()> {
        DeploymentStateStore::mark_callbacks_delivered(self, callback_ids, delivered_at_unix_ms)
            .await
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{ContextId, ExternalId, MessageId, TaskId};

    use super::*;

    #[tokio::test]
    async fn opens_and_lists_empty() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let records = store.list_deployments().await.unwrap();
        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn save_and_remove_roundtrip() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let record = DeploymentRecord {
            content_hash: "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            agent_name: "clickup-agent".to_string(),
            deployed_at: "2026-03-25T16:30:00Z".to_string(),
            status: DeploymentStatus::Active,
            last_error: None,
            last_attempt_at: Some("2026-03-25T16:30:00Z".to_string()),
            failure_count: 0,
        };

        store.save_deployment(&record).await.unwrap();
        let records = store.list_deployments().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], record);

        let removed = store
            .remove_deployment(
                &"1111111111111111111111111111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(removed);
        let removed_again = store
            .remove_deployment(
                &"1111111111111111111111111111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!removed_again);
    }

    #[tokio::test]
    async fn persists_across_reopen_on_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let store = DeploymentStateStore::open(&path).await.unwrap();
        let record = DeploymentRecord {
            content_hash: "2222222222222222222222222222222222222222222222222222222222222222"
                .parse()
                .unwrap(),
            agent_name: "persist-agent".to_string(),
            deployed_at: "2026-03-25T17:00:00Z".to_string(),
            status: DeploymentStatus::Failed,
            last_error: Some("boot failed".to_string()),
            last_attempt_at: Some("2026-03-25T17:00:00Z".to_string()),
            failure_count: 1,
        };
        store.save_deployment(&record).await.unwrap();
        drop(store);

        let reopened = {
            let mut reopened = None;
            for _ in 0..20 {
                match DeploymentStateStore::open(&path).await {
                    Ok(store) => {
                        reopened = Some(store);
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
                }
            }
            reopened.expect("reopen deployment state store after lock release")
        };
        let mut records = Vec::new();
        for _ in 0..50 {
            records = reopened.list_deployments().await.unwrap();
            if !records.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].content_hash.as_str(),
            "2222222222222222222222222222222222222222222222222222222222222222"
        );
        assert_eq!(records[0].agent_name, "persist-agent");
        assert_eq!(records[0].status, DeploymentStatus::Failed);
        assert_eq!(records[0].failure_count, 1);
    }

    /// Retry an async operation with bounded attempts and backoff.
    ///
    /// SurrealDB's embedded engine may hold file locks briefly after a
    /// connection is dropped, so reopen attempts need a short retry window.
    async fn retry_open(
        max_attempts: usize,
        delay_ms: u64,
        path: &std::path::Path,
    ) -> DeploymentStateStore {
        for _ in 0..max_attempts {
            match DeploymentStateStore::open(path).await {
                Ok(store) => return store,
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }
        }
        panic!("failed to reopen deployment state store after {max_attempts} attempts");
    }

    #[tokio::test]
    async fn event_producer_checkpoints_persist_across_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let store = DeploymentStateStore::open(&path).await.unwrap();
        store
            .save_event_producer_checkpoint("support/slack:C123ABC456", "cursor-42")
            .await
            .unwrap();
        drop(store);

        let reopened = retry_open(20, 100, &path).await;

        let checkpoints = reopened
            .list_event_producer_checkpoints()
            .await
            .expect("list checkpoints after reopen");

        assert_eq!(
            checkpoints
                .get("support/slack:C123ABC456")
                .map(String::as_str),
            Some("cursor-42")
        );
    }

    #[tokio::test]
    async fn scheduled_callbacks_persist_and_clear_after_delivery() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let store = DeploymentStateStore::open(&path).await.unwrap();
        let scheduled = store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "workflow-intake:resume".to_string(),
                dedupe_key: Some("resume-once".to_string()),
                payload: serde_json::json!({"goal": "resume"}),
                scheduled_for_unix_ms: 10,
                requested_at_unix_ms: 5,
                context_id: Some(ContextId::new(12, 2)),
                task_id: Some(TaskId::from_external(ExternalId::new("task-99"))),
                requesting_agent_id: Some("agent-42".to_string()),
                requesting_message_id: Some(MessageId::from("msg-77")),
            })
            .await
            .unwrap();
        let deduped = store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "workflow-intake:resume".to_string(),
                dedupe_key: Some("resume-once".to_string()),
                payload: serde_json::json!({"goal": "resume"}),
                scheduled_for_unix_ms: 11,
                requested_at_unix_ms: 6,
                context_id: Some(ContextId::new(12, 2)),
                task_id: Some(TaskId::from_external(ExternalId::new("task-99"))),
                requesting_agent_id: Some("agent-42".to_string()),
                requesting_message_id: Some(MessageId::from("msg-78")),
            })
            .await
            .unwrap();
        assert!(!deduped.created);
        assert_eq!(deduped.callback.callback_id, scheduled.callback.callback_id);
        drop(store);

        let reopened = retry_open(20, 100, &path).await;
        let due = reopened.list_due_callbacks(10, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].callback_id, scheduled.callback.callback_id);
        assert_eq!(due[0].source_key, "workflow-intake:resume");
        assert_eq!(due[0].requesting_agent_id.as_deref(), Some("agent-42"));

        reopened
            .mark_callbacks_delivered(std::slice::from_ref(&scheduled.callback.callback_id), 25)
            .await
            .unwrap();
        drop(reopened);

        let reopened = retry_open(20, 100, &path).await;
        let due = reopened.list_due_callbacks(1_000, 10).await.unwrap();
        assert!(due.is_empty());
    }
}
