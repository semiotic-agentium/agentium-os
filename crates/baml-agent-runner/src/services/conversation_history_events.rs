//! Conversation-history event service: notifies SSE subscribers when the transcript may have changed.
//!
//! Notifications fire (1) **after successful provenance commits** (runner wraps the graph writer)
//! and (2) on **A2A task
//! updates** (e.g. task-only status transitions that affect resume hints without new graph rows).

use std::sync::Arc;

use baml_rt_api::ConversationHistoryUpdate;
use tokio::{sync::broadcast, task::JoinHandle};

use crate::runner::AgentRunner;

pub(crate) struct ConversationHistoryEventServiceImpl {
    tx: broadcast::Sender<ConversationHistoryUpdate>,
    _tasks: Vec<JoinHandle<()>>,
}

impl ConversationHistoryEventServiceImpl {
    pub(crate) fn new(
        runner: Arc<AgentRunner>,
        tx: broadcast::Sender<ConversationHistoryUpdate>,
    ) -> Self {
        let mut handles = Vec::new();
        for mut updates in runner.subscribe_task_update_receivers() {
            let tx_clone = tx.clone();
            handles.push(tokio::spawn(async move {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(620);
                loop {
                    if std::time::Instant::now() >= deadline {
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
            }));
        }
        Self {
            tx,
            _tasks: handles,
        }
    }
}

impl baml_rt_api::ConversationHistoryEventService for ConversationHistoryEventServiceImpl {
    fn subscribe_updates(&self) -> broadcast::Receiver<ConversationHistoryUpdate> {
        self.tx.subscribe()
    }
}
