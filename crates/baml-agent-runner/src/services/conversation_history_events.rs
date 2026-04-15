//! Conversation-history event service wired to A2A task update broadcasts.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use baml_rt_api::ConversationHistoryUpdate;
use tokio::sync::broadcast;

use crate::runner::AgentRunner;

pub(crate) struct ConversationHistoryEventServiceImpl {
    runner: Arc<AgentRunner>,
}

impl ConversationHistoryEventServiceImpl {
    pub(crate) fn new(runner: Arc<AgentRunner>) -> Self {
        Self { runner }
    }
}

impl baml_rt_api::ConversationHistoryEventService for ConversationHistoryEventServiceImpl {
    fn subscribe_updates(&self) -> broadcast::Receiver<ConversationHistoryUpdate> {
        let (tx, rx) = broadcast::channel(1024);
        let receivers = self.runner.subscribe_task_update_receivers();
        for mut updates in receivers {
            let tx_clone = tx.clone();
            tokio::spawn(async move {
                let deadline = Instant::now() + Duration::from_secs(620);
                loop {
                    if Instant::now() >= deadline {
                        break;
                    }
                    match updates.recv().await {
                        Ok(event) => {
                            let context_id = event
                                .context_id()
                                .map(|id| id.as_str().to_string())
                                .unwrap_or_default();
                            if context_id.is_empty() {
                                continue;
                            }
                            let update = ConversationHistoryUpdate {
                                context_id,
                                task_id: event.task_id().map(|id| id.to_string()),
                            };
                            let _ = tx_clone.send(update);
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        rx
    }
}
