//! Host callback delivery deferral: gates emission until the scheduling A2A turn quiesces.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{CallbackDeliveryGate, Result, StoredCallback};

pub(crate) struct RunnerCallbackDeliveryGate {
    pub(crate) runner: Arc<crate::runner::AgentRunner>,
}

#[async_trait]
impl CallbackDeliveryGate for RunnerCallbackDeliveryGate {
    async fn can_emit_callback(&self, callback: &StoredCallback) -> Result<bool> {
        let Some(requesting_agent_id) = callback.requesting_agent_id.as_deref() else {
            return Ok(true);
        };
        let (Some(context_id), Some(task_id)) = (
            &callback.scheduling_context_id,
            &callback.scheduling_task_id,
        ) else {
            tracing::warn!(
                callback_id = %callback.callback_id,
                source_key = %callback.source_key,
                "callback missing scheduling scope; refusing emit (full cutover)"
            );
            return Ok(false);
        };
        let still_in_flight = self
            .runner
            .requesting_task_still_in_flight(requesting_agent_id, context_id, task_id)
            .await;
        if still_in_flight {
            tracing::debug!(
                callback_id = %callback.callback_id,
                requesting_agent_id,
                scheduling_context_id = %context_id,
                scheduling_task_id = %task_id,
                "callback delivery deferred: scheduling A2A turn still in flight"
            );
        }
        Ok(!still_in_flight)
    }
}
