//! Host contracts for `system/callback` persistence and delivery gating.
//!
//! # Architecture (proportional layering)
//!
//! Dependencies flow **inward** to this crate; nothing here depends on tool bundles.
//!
//! | Concern | Crate / location |
//! | --- | --- |
//! | [`CallbackStore`], [`StoredCallback`], [`ScheduleCallbackRequest`], [`CallbackDeliveryGate`] | **`baml-rt-core`** (this module) |
//! | Dispatch metadata keys (`schedulingContextId`, …) | [`crate::dispatch`] |
//! | Wall-clock millis for rows / producer payloads | [`crate::now_unix_ms`] |
//! | `install_callback_store` / `install_callback_delivery_gate` globals | **`baml-tools-system`** (`callback_store.rs`, `callback_delivery_gate.rs`) — service locator |
//! | `system/callback` tool + event producer | **`baml-tools-system`** (`callback_bundle`, `callback_producer`) |
//! | Surreal persistence implementing [`CallbackStore`] | **`baml-agent-runner`** (`deployment_state::DeploymentStateStore`) |
//!
//! The runner binary wires the store at startup by calling
//! `baml_tools_system::callback_store::install_callback_store` with an `Arc<dyn CallbackStore>`
//! backed by `DeploymentStateStore`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    BamlRtError, Result,
    ids::{ContextId, MessageId, TaskId},
};

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
    /// Dispatch scope (child context for detached).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// A2A turn that scheduled the callback; used for delivery deferral only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduling_context_id: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduling_task_id: Option<TaskId>,
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
    pub scheduling_context_id: Option<ContextId>,
    pub scheduling_task_id: Option<TaskId>,
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
/// ## Dispatch vs scheduling scope
///
/// - **`context_id` / `task_id`**: **Dispatch** scope on the emitted event / dispatch request.
///   For **detached** continuation the host mints a **child** context and task; for
///   **resume_current_task** they match the active A2A task.
/// - **`scheduling_context_id` / `scheduling_task_id`**: **Scheduling** scope — the A2A turn
///   that called `system/callback`. The runner delivery gate uses **only** these (with
///   `requesting_agent_id`) for in-flight deferral.
///
/// Delivery is intentionally at-least-once. The callback producer emits due callbacks,
/// then marks them delivered only after the host persists the producer checkpoint and the
/// next poll reconciles that checkpoint. Implementors must preserve pending callbacks
/// across crashes/restarts until `mark_callbacks_delivered` is called. Pending callbacks
/// may be marked as emitted before that reconciliation step; those rows must still remain
/// eligible for crash-recovery redelivery, but they should no longer participate in
/// dedupe/cancel lookups. Consuming agents should treat callback payloads as idempotent.
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

    async fn mark_callbacks_emitted(
        &self,
        callback_ids: &[String],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<String>>;

    async fn mark_callbacks_delivered(
        &self,
        callback_ids: &[String],
        delivered_at_unix_ms: u64,
    ) -> Result<()>;
}

/// Host-installed gate for deciding whether a due callback may be emitted now.
///
/// This is intentionally optional. Hosts that do not install a gate get the
/// default behavior: every due callback is eligible for delivery immediately.
#[async_trait]
pub trait CallbackDeliveryGate: Send + Sync {
    /// Return true when the callback may be emitted on this poll cycle.
    async fn can_emit_callback(&self, callback: &StoredCallback) -> Result<bool>;
}

/// Used by `baml-tools-system` when the global callback store is missing.
pub fn callback_store_not_installed() -> BamlRtError {
    BamlRtError::InvalidArgument("system/callback store is not installed in this host".to_string())
}
