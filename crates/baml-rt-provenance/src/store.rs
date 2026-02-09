use crate::error::Result;
use crate::events::ProvEvent;
use crate::normalizer::validate_event;
use async_trait::async_trait;
use tokio::sync::RwLock;

#[async_trait]
pub trait ProvenanceWriter: Send + Sync {
    async fn add_event(&self, event: ProvEvent) -> Result<()>;

    async fn add_events(&self, events: Vec<ProvEvent>) -> Result<()> {
        for event in events {
            self.add_event(event).await?;
        }
        Ok(())
    }

    async fn add_event_with_logging(&self, event: ProvEvent, context: &str) {
        if let Err(e) = self.add_event(event).await {
            tracing::warn!(error = ?e, context = context, "Failed to record provenance event");
        }
    }
}

pub struct InMemoryProvenanceStore {
    events: RwLock<Vec<ProvEvent>>,
}

impl InMemoryProvenanceStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    pub async fn events(&self) -> Vec<ProvEvent> {
        let events = self.events.read().await;
        let mut cloned = events.clone();
        cloned.sort_by(|a, b| a.id().cmp(b.id()));
        cloned
    }
}

impl Default for InMemoryProvenanceStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProvenanceWriter for InMemoryProvenanceStore {
    async fn add_event(&self, event: ProvEvent) -> Result<()> {
        validate_event(&event)?;
        let mut events = self.events.write().await;
        events.push(event);
        Ok(())
    }
}
