use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result,
    ids::{ContextId, MessageId, TaskId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{error, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredCallback {
    pub callback_id: String,
    pub source_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    pub payload: Value,
    pub scheduled_for_unix_ms: u64,
    pub requested_at_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requesting_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requesting_message_id: Option<MessageId>,
}

#[derive(Debug, Clone)]
pub struct ScheduleCallbackRequest {
    pub source_key: String,
    pub dedupe_key: Option<String>,
    pub payload: Value,
    pub scheduled_for_unix_ms: u64,
    pub requested_at_unix_ms: u64,
    pub context_id: Option<ContextId>,
    pub task_id: Option<TaskId>,
    pub requesting_agent_id: Option<String>,
    pub requesting_message_id: Option<MessageId>,
}

#[derive(Debug, Clone)]
pub struct ScheduleCallbackResult {
    pub callback: StoredCallback,
    pub created: bool,
}

#[derive(Debug, Clone)]
pub enum CancelCallbackSelector {
    CallbackId(String),
    DedupeKey {
        source_key: String,
        dedupe_key: String,
    },
}

/// Durable store backing `system/callback`.
///
/// Delivery is intentionally at-least-once. The callback producer emits due callbacks,
/// then marks them delivered only after the host persists the producer checkpoint and the
/// next poll reconciles that checkpoint. Implementors must preserve pending callbacks
/// across crashes/restarts until `mark_callbacks_delivered` is called, and consuming
/// agents should treat callback payloads as idempotent.
#[async_trait]
pub trait CallbackStore: Send + Sync {
    async fn schedule_callback(
        &self,
        request: ScheduleCallbackRequest,
    ) -> Result<ScheduleCallbackResult>;

    async fn cancel_callback(
        &self,
        selector: CancelCallbackSelector,
    ) -> Result<Option<StoredCallback>>;

    async fn list_due_callbacks(
        &self,
        now_unix_ms: u64,
        limit: usize,
    ) -> Result<Vec<StoredCallback>>;

    async fn mark_callbacks_delivered(
        &self,
        callback_ids: &[String],
        delivered_at_unix_ms: u64,
    ) -> Result<()>;
}

fn callback_store_slot() -> &'static RwLock<Option<Arc<dyn CallbackStore>>> {
    static STORE: OnceLock<RwLock<Option<Arc<dyn CallbackStore>>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(None))
}

pub fn install_callback_store(store: Arc<dyn CallbackStore>) {
    let mut guard = callback_store_slot().write().unwrap_or_else(|poisoned| {
        error!("callback store registry write lock poisoned; recovering inner state");
        poisoned.into_inner()
    });
    if guard.is_some() {
        warn!(
            "callback store replaced; pending callbacks in the previous store may be unreachable"
        );
    }
    *guard = Some(store);
}

pub fn callback_store() -> Option<Arc<dyn CallbackStore>> {
    callback_store_slot()
        .read()
        .unwrap_or_else(|poisoned| {
            error!("callback store registry read lock poisoned; recovering inner state");
            poisoned.into_inner()
        })
        .clone()
}

pub fn require_callback_store() -> Result<Arc<dyn CallbackStore>> {
    callback_store().ok_or_else(|| {
        BamlRtError::InvalidArgument(
            "system/callback store is not installed in this host".to_string(),
        )
    })
}

pub fn clear_callback_store() {
    let mut guard = callback_store_slot().write().unwrap_or_else(|poisoned| {
        error!("callback store registry clear lock poisoned; recovering inner state");
        poisoned.into_inner()
    });
    *guard = None;
}
