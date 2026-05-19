//! Wraps a [`ProvenanceWriter`] and notifies subscribers after each committed context-scoped write.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_conversation::view::{ProvenanceContextMessage, ProvenanceConversationContextItem};
use baml_rt_core::{
    ConversationHistoryUpdate,
    ids::{ContextId, TaskId},
};
use baml_rt_provenance::{ProvEvent, ProvenanceContextReader, ProvenanceWriter, error::Result};
use tokio::sync::broadcast;

/// Delegates all reads/writes to `inner` and sends [`ConversationHistoryUpdate`] after each
/// successful [`ProvenanceWriter::add_event`] that carries a [`ContextId`].
pub struct NotifyingProvenanceWriter {
    inner: Arc<dyn ProvenanceWriter>,
    notify_tx: broadcast::Sender<ConversationHistoryUpdate>,
}

impl NotifyingProvenanceWriter {
    pub fn new(
        inner: Arc<dyn ProvenanceWriter>,
        notify_tx: broadcast::Sender<ConversationHistoryUpdate>,
    ) -> Self {
        Self { inner, notify_tx }
    }
}

#[async_trait]
impl ProvenanceContextReader for NotifyingProvenanceWriter {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        self.inner.context_messages(context_id, limit).await
    }

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.inner.conversation_context(context_id, limit).await
    }

    async fn conversation_context_with_task(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.inner
            .conversation_context_with_task(context_id, limit, task_id)
            .await
    }
}

#[async_trait]
impl ProvenanceWriter for NotifyingProvenanceWriter {
    async fn add_event(&self, event: ProvEvent) -> Result<()> {
        let context_id = event.context_id_opt().map(|c| c.as_str().to_string());
        let task_id = event.task_id().map(|t| t.as_str().to_string());
        self.inner.add_event(event).await?;
        if let Some(context_id) = context_id {
            let _ = self.notify_tx.send(ConversationHistoryUpdate {
                context_id,
                task_id,
            });
        }
        Ok(())
    }
}
