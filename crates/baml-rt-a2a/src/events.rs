use crate::a2a_store::TaskUpdateEvent;
use async_trait::async_trait;
use tokio::sync::broadcast;

#[async_trait]
pub trait EventEmitter: Send + Sync {
    async fn emit(&self, event: TaskUpdateEvent);
}

pub struct BroadcastEventEmitter {
    tx: broadcast::Sender<TaskUpdateEvent>,
}

impl BroadcastEventEmitter {
    pub fn new(tx: broadcast::Sender<TaskUpdateEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl EventEmitter for BroadcastEventEmitter {
    async fn emit(&self, event: TaskUpdateEvent) {
        if self.tx.send(event).is_err() {
            tracing::debug!(
                "Task update broadcast skipped: no subscribers (fire-and-forget is acceptable)"
            );
        }
    }
}
