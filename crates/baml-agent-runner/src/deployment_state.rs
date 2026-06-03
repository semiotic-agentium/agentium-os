// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, path::Path, sync::Arc};

use baml_rt_core::{
    BamlRtError, DeploymentContentHash, DeploymentRecord, DeploymentStatus, Result,
    callback_store::{
        CallbackStore, CancelCallbackSelector, ScheduleCallbackRequest, ScheduleCallbackResult,
        StoredCallback,
    },
    clock_events,
    event_subscription::EventSourceKey,
    ingress_store::{IngressId, IngressItem, IngressStore},
    now_unix_ms,
};
use serde::Deserialize;
#[cfg(test)]
use surrealdb::engine::local::Mem;
use surrealdb::{
    Surreal,
    engine::local::{Db, SurrealKv},
};
use tracing::{debug, warn};
use uuid::Uuid;

const NS: &str = "baml";
const DB_NAME: &str = "runner_state";
const TBL_DEPLOYMENTS: &str = "deployments";
const TBL_EVENT_PRODUCER_CHECKPOINTS: &str = "event_producer_checkpoints";
const TBL_SCHEDULED_CALLBACKS: &str = "scheduled_callbacks";
const TBL_INGRESS_INBOX: &str = "ingress_inbox";

/// Hard ceiling on how long a single `DeploymentStateStore::open` call will
/// wait to acquire the on-disk database lock. The embedded engine releases
/// the file lock from a background driver task, so a `drop`-then-reopen
/// pattern can observe the prior lock as still held; without a timeout the
/// call blocks inside `.await` instead of returning an error, which leaves
/// the existing `retry_open` exponential-backoff loop unable to make
/// progress. A bounded timeout converts the hang into a retryable
/// [`BamlRtError::Io`]. The value is generous (well above any plausible
/// cold-open latency under heavy CI parallelism) because the timeout is an
/// emergency stop for the indefinite-hang failure mode, not a budget for
/// the normal case — a healthy cold-open completes in tens of milliseconds.
const OPEN_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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
    "DEFINE FIELD IF NOT EXISTS emitted_at_unix_ms ON scheduled_callbacks TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS delivered_at_unix_ms ON scheduled_callbacks TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS cancelled_at_unix_ms ON scheduled_callbacks TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS context_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS task_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS requesting_agent_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS requesting_message_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS scheduling_context_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE FIELD IF NOT EXISTS scheduling_task_id ON scheduled_callbacks TYPE option<string>",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_id ON scheduled_callbacks FIELDS callback_id UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_due ON scheduled_callbacks FIELDS status, scheduled_for_unix_ms",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_dedupe ON scheduled_callbacks FIELDS source_key, dedupe_key, status",
    "DEFINE INDEX IF NOT EXISTS idx_scheduled_callback_dedupe_unemitted ON scheduled_callbacks FIELDS source_key, dedupe_key, status, emitted_at_unix_ms",
    "DEFINE TABLE IF NOT EXISTS ingress_inbox SCHEMAFULL",
    "DEFINE FIELD IF NOT EXISTS ingress_id ON ingress_inbox TYPE string",
    "DEFINE FIELD IF NOT EXISTS source_key ON ingress_inbox TYPE string",
    "DEFINE FIELD IF NOT EXISTS payload_json ON ingress_inbox TYPE string",
    "DEFINE FIELD IF NOT EXISTS status ON ingress_inbox TYPE string",
    "DEFINE FIELD IF NOT EXISTS enqueued_at_unix_ms ON ingress_inbox TYPE int",
    "DEFINE FIELD IF NOT EXISTS emitted_at_unix_ms ON ingress_inbox TYPE option<int>",
    "DEFINE FIELD IF NOT EXISTS delivered_at_unix_ms ON ingress_inbox TYPE option<int>",
    "DEFINE INDEX IF NOT EXISTS idx_ingress_id ON ingress_inbox FIELDS ingress_id UNIQUE",
    "DEFINE INDEX IF NOT EXISTS idx_ingress_pending ON ingress_inbox FIELDS status, enqueued_at_unix_ms",
    "DEFINE INDEX IF NOT EXISTS idx_ingress_emitted ON ingress_inbox FIELDS status, emitted_at_unix_ms",
];

pub struct DeploymentStateStore {
    db: Arc<Surreal<Db>>,
    /// Guards the read-then-write paths in `schedule_callback` and
    /// `cancel_callback` to prevent concurrent mutations on the same row.
    callback_lock: tokio::sync::Mutex<()>,
    /// Guards ingress inbox read-then-write paths.
    ingress_lock: tokio::sync::Mutex<()>,
}

impl DeploymentStateStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().into_owned();
        let db = tokio::time::timeout(
            OPEN_LOCK_TIMEOUT,
            Surreal::new::<SurrealKv>(path_str.as_str()),
        )
        .await
        .map_err(|_| {
            let secs = OPEN_LOCK_TIMEOUT.as_secs();
            BamlRtError::Io(std::io::Error::other(format!(
                "timed out after {secs}s acquiring on-disk database lock at {path_str}"
            )))
        })?
        .map_err(to_write_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(to_write_err)?;
        let store = Self {
            db: Arc::new(db),
            callback_lock: tokio::sync::Mutex::new(()),
            ingress_lock: tokio::sync::Mutex::new(()),
        };
        store.init_schema().await?;
        Ok(store)
    }

    /// In-memory Surreal backend for unit tests in this crate.
    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self> {
        let db = Surreal::new::<Mem>(()).await.map_err(to_write_err)?;
        db.use_ns(NS).use_db(DB_NAME).await.map_err(to_write_err)?;
        let store = Self {
            db: Arc::new(db),
            callback_lock: tokio::sync::Mutex::new(()),
            ingress_lock: tokio::sync::Mutex::new(()),
        };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> Result<()> {
        for stmt in SCHEMA_QUERIES {
            self.db.query(*stmt).await.map_err(to_write_err)?;
        }
        self.purge_pre_cutover_pending_callbacks().await?;
        Ok(())
    }

    /// Full cutover: pending rows without scheduling scope cannot be delivered safely.
    async fn purge_pre_cutover_pending_callbacks(&self) -> Result<()> {
        let mut resp = self
            .db
            .query(format!(
                "DELETE FROM {TBL_SCHEDULED_CALLBACKS} WHERE status = 'pending' \
                 AND (scheduling_context_id IS NONE OR scheduling_task_id IS NONE) RETURN BEFORE"
            ))
            .await
            .map_err(to_write_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        if !rows.is_empty() {
            warn!(
                removed = rows.len(),
                "removed pre-cutover pending scheduled_callbacks rows missing scheduling scope"
            );
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

    // ── Generic ingress inbox ──────────────────────────────────────────

    pub async fn enqueue_ingress_item(&self, item: &IngressItem) -> Result<bool> {
        let _guard = self.ingress_lock.lock().await;

        let mut resp = self
            .db
            .query(format!(
                "SELECT ingress_id FROM {TBL_INGRESS_INBOX} \
                 WHERE ingress_id = $ingress_id LIMIT 1"
            ))
            .bind(("ingress_id", item.ingress_id.to_string()))
            .await
            .map_err(to_read_err)?;
        let existing_rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        if !existing_rows.is_empty() {
            return Ok(false);
        }

        self.db
            .query(format!(
                "CREATE {TBL_INGRESS_INBOX} SET \
                    ingress_id = $ingress_id, \
                    source_key = $source_key, \
                    payload_json = $payload_json, \
                    status = 'pending', \
                    enqueued_at_unix_ms = $enqueued_at_unix_ms, \
                    emitted_at_unix_ms = NONE, \
                    delivered_at_unix_ms = NONE"
            ))
            .bind(("ingress_id", item.ingress_id.to_string()))
            .bind(("source_key", item.source_key.to_string()))
            .bind(("payload_json", item.payload_json.clone()))
            .bind((
                "enqueued_at_unix_ms",
                unix_ms_to_db_int(item.enqueued_at_unix_ms, "ingress enqueued_at_unix_ms")?,
            ))
            .await
            .map_err(to_write_err)?;
        Ok(true)
    }

    /// Lock-free read: callers must tolerate stale results because
    /// `mark_ingress_emitted` does the authoritative claim under `ingress_lock`.
    pub async fn list_pending_ingress_items(&self, limit: usize) -> Result<Vec<IngressItem>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT ingress_id,source_key,payload_json,enqueued_at_unix_ms \
                 FROM {TBL_INGRESS_INBOX} \
                 WHERE status = 'pending' AND emitted_at_unix_ms = NONE \
                 ORDER BY enqueued_at_unix_ms ASC, ingress_id ASC LIMIT $limit"
            ))
            .bind(("limit", limit as i64))
            .await
            .map_err(to_read_err)?;
        let rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        rows.into_iter().map(parse_ingress_row).collect()
    }

    pub async fn requeue_stale_ingress(&self, emitted_before_unix_ms: u64) -> Result<usize> {
        let _guard = self.ingress_lock.lock().await;
        let mut resp = self
            .db
            .query(format!(
                "UPDATE {TBL_INGRESS_INBOX} SET \
                    status = 'pending', \
                    emitted_at_unix_ms = NONE \
                 WHERE status = 'emitted' \
                   AND emitted_at_unix_ms != NONE \
                   AND emitted_at_unix_ms <= $emitted_before_unix_ms"
            ))
            .bind((
                "emitted_before_unix_ms",
                unix_ms_to_db_int(emitted_before_unix_ms, "ingress emitted_before_unix_ms")?,
            ))
            .await
            .map_err(to_write_err)?;
        let updated_rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        Ok(updated_rows.len())
    }

    pub async fn mark_ingress_emitted(
        &self,
        ingress_ids: &[IngressId],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<IngressId>> {
        if ingress_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ingress_ids = ingress_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let _guard = self.ingress_lock.lock().await;

        // Single UPDATE — returned rows are exactly the ones that were eligible
        // and claimed. No separate SELECT needed.
        let mut resp = self
            .db
            .query(format!(
                "UPDATE {TBL_INGRESS_INBOX} SET \
                    status = 'emitted', \
                    emitted_at_unix_ms = $emitted_at_unix_ms \
                 WHERE ingress_id INSIDE $ingress_ids \
                   AND status = 'pending' \
                   AND emitted_at_unix_ms = NONE"
            ))
            .bind(("ingress_ids", ingress_ids))
            .bind((
                "emitted_at_unix_ms",
                unix_ms_to_db_int(emitted_at_unix_ms, "ingress emitted_at_unix_ms")?,
            ))
            .await
            .map_err(to_write_err)?;
        let updated_rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;

        updated_rows
            .into_iter()
            .map(|row| {
                #[derive(Deserialize)]
                struct EmittedRow {
                    ingress_id: IngressId,
                }
                let parsed: EmittedRow = serde_json::from_value(row).map_err(|err| {
                    BamlRtError::InvalidArgument(format!(
                        "invalid ingress emission row from state DB: {err}"
                    ))
                })?;
                Ok(parsed.ingress_id)
            })
            .collect()
    }

    pub async fn mark_ingress_delivered(
        &self,
        ingress_ids: &[IngressId],
        delivered_at_unix_ms: u64,
    ) -> Result<()> {
        if ingress_ids.is_empty() {
            return Ok(());
        }
        let ingress_ids = ingress_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let _guard = self.ingress_lock.lock().await;

        let mut resp = self
            .db
            .query(format!(
                "UPDATE {TBL_INGRESS_INBOX} SET \
                    status = 'delivered', \
                    delivered_at_unix_ms = $delivered_at_unix_ms \
                 WHERE ingress_id INSIDE $ingress_ids \
                   AND status INSIDE ['pending', 'emitted']"
            ))
            .bind(("ingress_ids", ingress_ids.to_vec()))
            .bind((
                "delivered_at_unix_ms",
                unix_ms_to_db_int(delivered_at_unix_ms, "ingress delivered_at_unix_ms")?,
            ))
            .await
            .map_err(to_write_err)?;
        let updated_rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        if updated_rows.len() < ingress_ids.len() {
            warn!(
                requested_count = ingress_ids.len(),
                updated_count = updated_rows.len(),
                "some ingress items were not pending at delivery time"
            );
        }
        Ok(())
    }

    pub async fn schedule_callback(
        &self,
        request: &ScheduleCallbackRequest,
    ) -> Result<ScheduleCallbackResult> {
        // When a dedupe key is present, atomically check-then-insert inside a
        // transaction to avoid a TOCTOU race between concurrent schedule calls.
        if let Some(dedupe_key) = &request.dedupe_key {
            return self
                .schedule_callback_with_dedupe(request, dedupe_key)
                .await;
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
            scheduling_context_id: request.scheduling_context_id.clone(),
            scheduling_task_id: request.scheduling_task_id.clone(),
            requesting_agent_id: request.requesting_agent_id.clone(),
            requesting_message_id: request.requesting_message_id.clone(),
        };
        self.upsert_callback_row(&callback, "pending", None, None, None)
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

    /// Dedupe-check and insert a callback, holding `callback_lock` to prevent
    /// concurrent schedule calls with the same key from both inserting.
    async fn schedule_callback_with_dedupe(
        &self,
        request: &ScheduleCallbackRequest,
        dedupe_key: &str,
    ) -> Result<ScheduleCallbackResult> {
        let _guard = self.callback_lock.lock().await;

        if let Some(existing) = self
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
            scheduling_context_id: request.scheduling_context_id.clone(),
            scheduling_task_id: request.scheduling_task_id.clone(),
            requesting_agent_id: request.requesting_agent_id.clone(),
            requesting_message_id: request.requesting_message_id.clone(),
        };
        self.upsert_callback_row(&callback, "pending", None, None, None)
            .await?;
        debug!(
            callback_id = %callback.callback_id,
            source_key = %callback.source_key,
            dedupe_key = %dedupe_key,
            scheduled_for_unix_ms = callback.scheduled_for_unix_ms,
            "runner state stored scheduled callback (dedupe key checked, no match)"
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
        let _guard = self.callback_lock.lock().await;
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
            None,
            Some(now_unix_ms(clock_events::SYSTEM_CALLBACK_CANCEL)),
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
                "SELECT callback_id,source_key,dedupe_key,payload_json,scheduled_for_unix_ms,requested_at_unix_ms,context_id,task_id,scheduling_context_id,scheduling_task_id,requesting_agent_id,requesting_message_id \
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

        let mut resp = self
            .db
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
        let updated_rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        let updated_count = updated_rows.len();
        let requested_count = callback_ids.len();
        if updated_count < requested_count {
            warn!(
                requested_count,
                updated_count,
                "some callbacks were not pending at delivery time; \
                 they may have been cancelled between emission and reconciliation"
            );
        }
        debug!(
            updated_count,
            requested_count, delivered_at_unix_ms, "runner state marked callbacks delivered"
        );
        Ok(())
    }

    pub async fn mark_callbacks_emitted(
        &self,
        callback_ids: &[String],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<String>> {
        if callback_ids.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.callback_lock.lock().await;

        #[derive(Deserialize)]
        struct CallbackEmissionRow {
            callback_id: String,
            #[serde(default)]
            emitted_at_unix_ms: Option<i64>,
        }

        let mut resp = self
            .db
            .query(format!(
                "SELECT callback_id,emitted_at_unix_ms \
                 FROM {TBL_SCHEDULED_CALLBACKS} \
                 WHERE callback_id INSIDE $callback_ids \
                   AND status = 'pending'"
            ))
            .bind(("callback_ids", callback_ids.to_vec()))
            .await
            .map_err(to_read_err)?;
        let pending_rows: Vec<serde_json::Value> = resp.take(0).map_err(to_read_err)?;
        let emission_rows = pending_rows
            .into_iter()
            .map(|row| {
                let parsed: CallbackEmissionRow = serde_json::from_value(row).map_err(|err| {
                    BamlRtError::InvalidArgument(format!(
                        "invalid callback emission row from state DB: {err}"
                    ))
                })?;
                Ok(parsed)
            })
            .collect::<Result<Vec<_>>>()?;
        let eligible_ids = emission_rows
            .iter()
            .map(|row| row.callback_id.clone())
            .collect::<Vec<_>>();
        let first_emission_ids = emission_rows
            .into_iter()
            .filter(|row| row.emitted_at_unix_ms.is_none())
            .map(|row| row.callback_id)
            .collect::<Vec<_>>();
        if !first_emission_ids.is_empty() {
            self.db
                .query(format!(
                    "UPDATE {TBL_SCHEDULED_CALLBACKS} SET \
                        emitted_at_unix_ms = $emitted_at_unix_ms \
                     WHERE callback_id INSIDE $callback_ids \
                       AND status = 'pending' \
                       AND emitted_at_unix_ms = NONE"
                ))
                .bind(("callback_ids", first_emission_ids.clone()))
                .bind(("emitted_at_unix_ms", emitted_at_unix_ms as i64))
                .await
                .map_err(to_write_err)?;
        }
        if eligible_ids.len() < callback_ids.len() {
            debug!(
                requested_count = callback_ids.len(),
                emitted_count = eligible_ids.len(),
                "some callbacks were no longer eligible for emission claim"
            );
        }
        Ok(eligible_ids)
    }

    async fn find_pending_callback_by_id(
        &self,
        callback_id: &str,
    ) -> Result<Option<StoredCallback>> {
        let mut resp = self
            .db
            .query(format!(
                "SELECT callback_id,source_key,dedupe_key,payload_json,scheduled_for_unix_ms,requested_at_unix_ms,context_id,task_id,scheduling_context_id,scheduling_task_id,requesting_agent_id,requesting_message_id \
                 FROM {TBL_SCHEDULED_CALLBACKS} \
                 WHERE callback_id = $callback_id AND status = 'pending' AND emitted_at_unix_ms = NONE LIMIT 1"
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
                "SELECT callback_id,source_key,dedupe_key,payload_json,scheduled_for_unix_ms,requested_at_unix_ms,context_id,task_id,scheduling_context_id,scheduling_task_id,requesting_agent_id,requesting_message_id \
                 FROM {TBL_SCHEDULED_CALLBACKS} \
                 WHERE source_key = $source_key AND dedupe_key = $dedupe_key AND status = 'pending' AND emitted_at_unix_ms = NONE LIMIT 1"
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
        emitted_at_unix_ms: Option<u64>,
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
                    emitted_at_unix_ms = $emitted_at_unix_ms, \
                    delivered_at_unix_ms = $delivered_at_unix_ms, \
                    cancelled_at_unix_ms = $cancelled_at_unix_ms, \
                    context_id = $context_id, \
                    task_id = $task_id, \
                    scheduling_context_id = $scheduling_context_id, \
                    scheduling_task_id = $scheduling_task_id, \
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
                "emitted_at_unix_ms",
                emitted_at_unix_ms.map(|value| value as i64),
            ))
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
            .bind((
                "scheduling_context_id",
                callback
                    .scheduling_context_id
                    .as_ref()
                    .map(|value| value.as_str().to_string()),
            ))
            .bind((
                "scheduling_task_id",
                callback
                    .scheduling_task_id
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
        scheduling_context_id: Option<String>,
        #[serde(default)]
        scheduling_task_id: Option<String>,
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
        scheduling_context_id: parsed
            .scheduling_context_id
            .map(|value| baml_rt_core::ids::ContextId::from(value.as_str())),
        scheduling_task_id: parsed.scheduling_task_id.map(|value| {
            baml_rt_core::ids::TaskId::from_external(baml_rt_core::ids::ExternalId::new(value))
        }),
        requesting_agent_id: parsed.requesting_agent_id,
        requesting_message_id: parsed
            .requesting_message_id
            .map(baml_rt_core::ids::MessageId::from),
    })
}

fn parse_ingress_row(row: serde_json::Value) -> Result<IngressItem> {
    #[derive(Deserialize)]
    struct IngressRow {
        ingress_id: IngressId,
        source_key: EventSourceKey,
        payload_json: String,
        enqueued_at_unix_ms: i64,
    }

    let parsed: IngressRow = serde_json::from_value(row).map_err(|e| {
        BamlRtError::InvalidArgument(format!("invalid ingress row from state DB: {e}"))
    })?;

    Ok(IngressItem {
        ingress_id: parsed.ingress_id,
        source_key: parsed.source_key,
        payload_json: parsed.payload_json,
        enqueued_at_unix_ms: u64::try_from(parsed.enqueued_at_unix_ms).map_err(|_| {
            BamlRtError::InvalidArgument(format!(
                "negative enqueued_at_unix_ms ({value}) in ingress row from state DB",
                value = parsed.enqueued_at_unix_ms
            ))
        })?,
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

fn unix_ms_to_db_int(value: u64, field_name: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        BamlRtError::InvalidArgument(format!(
            "{field_name} exceeds signed 64-bit range for state DB storage"
        ))
    })
}

/// Surreal-backed [`CallbackStore`](baml_rt_core::CallbackStore) for runner state (`state.db`).
///
/// Registered at process start via [`install_callback_store`](baml_tools_system::callback_store::install_callback_store).
/// Timestamps use [`now_unix_ms`](baml_rt_core::now_unix_ms) from core so this
/// module does not depend on `baml-tools-system` for persistence-only clocking.
#[async_trait::async_trait]
impl CallbackStore for DeploymentStateStore {
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

    async fn mark_callbacks_emitted(
        &self,
        callback_ids: &[String],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<String>> {
        DeploymentStateStore::mark_callbacks_emitted(self, callback_ids, emitted_at_unix_ms).await
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

/// Surreal-backed [`IngressStore`](baml_rt_core::IngressStore) for runner state (`state.db`).
///
/// Registered at process start via [`install_ingress_store`](baml_rt_tools::ingress_store::install_ingress_store).
#[async_trait::async_trait]
impl IngressStore for DeploymentStateStore {
    async fn enqueue(&self, item: &IngressItem) -> Result<bool> {
        DeploymentStateStore::enqueue_ingress_item(self, item).await
    }

    async fn list_pending(&self, limit: usize) -> Result<Vec<IngressItem>> {
        DeploymentStateStore::list_pending_ingress_items(self, limit).await
    }

    async fn requeue_stale(&self, emitted_before_unix_ms: u64) -> Result<usize> {
        DeploymentStateStore::requeue_stale_ingress(self, emitted_before_unix_ms).await
    }

    async fn mark_emitted(
        &self,
        ingress_ids: &[IngressId],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<IngressId>> {
        DeploymentStateStore::mark_ingress_emitted(self, ingress_ids, emitted_at_unix_ms).await
    }

    async fn mark_delivered(
        &self,
        ingress_ids: &[IngressId],
        delivered_at_unix_ms: u64,
    ) -> Result<()> {
        DeploymentStateStore::mark_ingress_delivered(self, ingress_ids, delivered_at_unix_ms).await
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::{
        event_subscription::EventSourceKey,
        ids::{ContextId, ExternalId, MessageId, TaskId},
        ingress_store::{IngressId, IngressItem},
    };

    use super::*;

    #[test]
    fn unix_ms_to_db_int_rejects_overflow() {
        let err = unix_ms_to_db_int((i64::MAX as u64) + 1, "test timestamp")
            .expect_err("overflowing timestamps should be rejected");
        assert!(
            err.to_string()
                .contains("test timestamp exceeds signed 64-bit range")
        );
    }

    #[tokio::test]
    async fn deployment_store_lifecycle_matrix() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        assert!(store.list_deployments().await.unwrap().is_empty());

        let hash: DeploymentContentHash =
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap();
        let record = DeploymentRecord {
            content_hash: hash.clone(),
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

        assert!(store.remove_deployment(&hash).await.unwrap());
        assert!(!store.remove_deployment(&hash).await.unwrap());
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

        let reopened = retry_open(&path).await;
        let records = reopened.list_deployments().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].content_hash.as_str(),
            "2222222222222222222222222222222222222222222222222222222222222222"
        );
        assert_eq!(records[0].agent_name, "persist-agent");
        assert_eq!(records[0].status, DeploymentStatus::Failed);
        assert_eq!(records[0].failure_count, 1);
    }

    /// Retry opening a `DeploymentStateStore` with exponential backoff.
    ///
    /// SurrealDB's embedded engine may hold file locks briefly after a
    /// connection is dropped. On slow CI runners the delay can exceed a
    /// constant-interval budget, so we start at 50 ms and double each
    /// attempt (capped at 2 s) for a total budget of ~20 s.
    async fn retry_open(path: &std::path::Path) -> DeploymentStateStore {
        let mut delay = std::time::Duration::from_millis(50);
        let max_delay = std::time::Duration::from_secs(2);
        let max_attempts = 10;
        for attempt in 1..=max_attempts {
            match DeploymentStateStore::open(path).await {
                Ok(store) => return store,
                Err(e) => {
                    if attempt == max_attempts {
                        panic!(
                            "failed to reopen deployment state store after {max_attempts} attempts: {e}"
                        );
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(max_delay);
                }
            }
        }
        unreachable!()
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

        let reopened = retry_open(&path).await;

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
    async fn list_due_callbacks_immediately_after_schedule_preserves_dispatch_task_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");
        let store = DeploymentStateStore::open(&path).await.unwrap();
        let dispatch_task =
            TaskId::from_external(ExternalId::new("dispatch-task-immediate".to_string()));
        let dispatch_ctx = ContextId::new(900, 1);
        let sched_ctx = ContextId::new(900, 2);
        let sched_task = TaskId::from_external(ExternalId::new("sched-task-immediate".to_string()));
        store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "dispatch-echo:callback:immediate-test".into(),
                dedupe_key: None,
                payload: serde_json::json!({ "probe": true }),
                scheduled_for_unix_ms: 0,
                requested_at_unix_ms: 0,
                context_id: Some(dispatch_ctx.clone()),
                task_id: Some(dispatch_task.clone()),
                scheduling_context_id: Some(sched_ctx),
                scheduling_task_id: Some(sched_task),
                requesting_agent_id: Some("agent-immediate".into()),
                requesting_message_id: None,
            })
            .await
            .unwrap();
        let due = store.list_due_callbacks(1_000_000, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(
            due[0].task_id.as_ref().map(|t| t.as_str()),
            Some("dispatch-task-immediate")
        );
        assert_eq!(
            due[0].context_id.as_ref().map(|c| c.as_str()),
            Some(dispatch_ctx.as_str())
        );
    }

    #[tokio::test]
    async fn scheduled_callbacks_persist_and_clear_after_delivery() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let store = DeploymentStateStore::open(&path).await.unwrap();
        let scheduled = store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "coordinator-agent:resume".to_string(),
                dedupe_key: Some("resume-once".to_string()),
                payload: serde_json::json!({"goal": "resume"}),
                scheduled_for_unix_ms: 10,
                requested_at_unix_ms: 5,
                context_id: Some(ContextId::new(12, 2)),
                task_id: Some(TaskId::from_external(ExternalId::new("task-99"))),
                scheduling_context_id: Some(ContextId::new(12, 2)),
                scheduling_task_id: Some(TaskId::from_external(ExternalId::new("task-99"))),
                requesting_agent_id: Some("agent-42".to_string()),
                requesting_message_id: Some(MessageId::from("msg-77")),
            })
            .await
            .unwrap();
        let deduped = store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "coordinator-agent:resume".to_string(),
                dedupe_key: Some("resume-once".to_string()),
                payload: serde_json::json!({"goal": "resume"}),
                scheduled_for_unix_ms: 11,
                requested_at_unix_ms: 6,
                context_id: Some(ContextId::new(12, 2)),
                task_id: Some(TaskId::from_external(ExternalId::new("task-99"))),
                scheduling_context_id: Some(ContextId::new(12, 2)),
                scheduling_task_id: Some(TaskId::from_external(ExternalId::new("task-99"))),
                requesting_agent_id: Some("agent-42".to_string()),
                requesting_message_id: Some(MessageId::from("msg-78")),
            })
            .await
            .unwrap();
        assert!(!deduped.created);
        assert_eq!(deduped.callback.callback_id, scheduled.callback.callback_id);
        drop(store);

        let reopened = retry_open(&path).await;
        let due = reopened.list_due_callbacks(10, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].callback_id, scheduled.callback.callback_id);
        assert_eq!(due[0].source_key, "coordinator-agent:resume");
        assert_eq!(due[0].requesting_agent_id.as_deref(), Some("agent-42"));
        assert_eq!(
            due[0].context_id.as_ref().map(|c| c.as_str()),
            Some("ctx-12-2")
        );
        assert_eq!(due[0].task_id.as_ref().map(|t| t.as_str()), Some("task-99"));
        assert_eq!(
            due[0].scheduling_context_id.as_ref().map(|c| c.as_str()),
            Some("ctx-12-2")
        );
        assert_eq!(
            due[0].scheduling_task_id.as_ref().map(|t| t.as_str()),
            Some("task-99")
        );

        reopened
            .mark_callbacks_delivered(std::slice::from_ref(&scheduled.callback.callback_id), 25)
            .await
            .unwrap();
        drop(reopened);

        let reopened = retry_open(&path).await;
        let due = reopened.list_due_callbacks(1_000, 10).await.unwrap();
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn emitted_callbacks_do_not_block_rescheduling_same_dedupe_key() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let first = store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "coordinator-agent:resume".to_string(),
                dedupe_key: Some("resume-once".to_string()),
                payload: serde_json::json!({"goal": "resume"}),
                scheduled_for_unix_ms: 10,
                requested_at_unix_ms: 5,
                context_id: None,
                task_id: None,
                scheduling_context_id: Some(ContextId::new(99, 1)),
                scheduling_task_id: Some(TaskId::from_external(ExternalId::new("sched-agent-42"))),
                requesting_agent_id: Some("agent-42".to_string()),
                requesting_message_id: None,
            })
            .await
            .unwrap();

        let due = store.list_due_callbacks(10, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].callback_id, first.callback.callback_id);

        let emitted_ids = store
            .mark_callbacks_emitted(std::slice::from_ref(&first.callback.callback_id), 15)
            .await
            .unwrap();
        assert_eq!(emitted_ids, vec![first.callback.callback_id.clone()]);

        let rescheduled = store
            .schedule_callback(&ScheduleCallbackRequest {
                source_key: "coordinator-agent:resume".to_string(),
                dedupe_key: Some("resume-once".to_string()),
                payload: serde_json::json!({"goal": "resume-again"}),
                scheduled_for_unix_ms: 20,
                requested_at_unix_ms: 16,
                context_id: None,
                task_id: None,
                scheduling_context_id: Some(ContextId::new(99, 1)),
                scheduling_task_id: Some(TaskId::from_external(ExternalId::new("sched-agent-42"))),
                requesting_agent_id: Some("agent-42".to_string()),
                requesting_message_id: None,
            })
            .await
            .unwrap();
        assert!(rescheduled.created);
        assert_ne!(rescheduled.callback.callback_id, first.callback.callback_id);

        let cancelled = store
            .cancel_callback(CancelCallbackSelector::CallbackId(
                first.callback.callback_id,
            ))
            .await
            .unwrap();
        assert!(
            cancelled.is_none(),
            "already-emitted callbacks must not be cancellable before reconciliation"
        );
    }

    fn sample_ingress_item(ingress_id: &str) -> IngressItem {
        IngressItem {
            ingress_id: IngressId::parse(ingress_id).expect("valid ingress id"),
            source_key: EventSourceKey::parse("slack:C123ABC456").expect("valid source key"),
            payload_json: r#"{"message":"hello"}"#.to_string(),
            enqueued_at_unix_ms: 1_775_512_000_000,
        }
    }

    #[tokio::test]
    async fn ingress_items_persist_across_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let store = DeploymentStateStore::open(&path).await.unwrap();
        let item = sample_ingress_item("ingress-generic-1");
        assert!(store.enqueue_ingress_item(&item).await.unwrap());
        assert!(!store.enqueue_ingress_item(&item).await.unwrap());
        drop(store);

        let reopened = retry_open(&path).await;
        let items = reopened.list_pending_ingress_items(10).await.unwrap();
        assert_eq!(items, vec![item]);
    }

    #[tokio::test]
    async fn ingress_emission_and_delivery_roundtrip() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let item = sample_ingress_item("ingress-generic-2");
        assert!(store.enqueue_ingress_item(&item).await.unwrap());

        let emitted = store
            .mark_ingress_emitted(std::slice::from_ref(&item.ingress_id), 15)
            .await
            .unwrap();
        assert_eq!(emitted, vec![item.ingress_id.clone()]);

        let pending = store.list_pending_ingress_items(10).await.unwrap();
        assert!(
            pending.is_empty(),
            "emitted ingress should not remain claimable until it is requeued or delivered"
        );

        store
            .mark_ingress_delivered(std::slice::from_ref(&item.ingress_id), 20)
            .await
            .unwrap();
        let pending = store.list_pending_ingress_items(10).await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn ingress_requeues_stale_emitted_items_after_timeout() {
        let store = DeploymentStateStore::open_in_memory().await.unwrap();
        let item = sample_ingress_item("ingress-generic-3");
        assert!(store.enqueue_ingress_item(&item).await.unwrap());

        let emitted = store
            .mark_ingress_emitted(std::slice::from_ref(&item.ingress_id), 15)
            .await
            .unwrap();
        assert_eq!(emitted, vec![item.ingress_id.clone()]);

        assert!(
            store
                .list_pending_ingress_items(10)
                .await
                .unwrap()
                .is_empty(),
            "freshly emitted ingress should not be visible to claimers"
        );

        let reclaimed = store.requeue_stale_ingress(14).await.unwrap();
        assert_eq!(reclaimed, 0);
        assert!(
            store
                .list_pending_ingress_items(10)
                .await
                .unwrap()
                .is_empty(),
            "ingress should not be requeued before the retry threshold"
        );

        let reclaimed = store.requeue_stale_ingress(15).await.unwrap();
        assert_eq!(reclaimed, 1);
        let pending = store.list_pending_ingress_items(10).await.unwrap();
        assert_eq!(pending, vec![item.clone()]);
    }
}
