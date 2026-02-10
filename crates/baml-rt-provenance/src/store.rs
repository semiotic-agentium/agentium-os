use crate::error::Result;
use crate::events::ProvEvent;
use async_trait::async_trait;
use baml_rt_core::ids::ContextId;
use serde_json::Value;

#[async_trait]
pub trait ProvenanceWriter: ProvenanceContextReader + Send + Sync {
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

#[derive(Debug, Clone)]
pub struct ProvenanceContextMessage {
    pub message_id: String,
    pub timestamp_ms: u64,
    pub role: String,
    pub content: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProvenanceConversationContextItem {
    pub timestamp_ms: u64,
    pub event_id: String,
    pub role: String,
    pub content: Value,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSessionPhase {
    Open,
    Send,
    Next,
    Finish,
    Abort,
    Unknown(String),
}

impl ToolSessionPhase {
    pub fn from_metadata(metadata: &Value) -> Self {
        let phase = metadata
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match phase {
            "open" => Self::Open,
            "send" => Self::Send,
            "next" => Self::Next,
            "finish" => Self::Finish,
            "abort" => Self::Abort,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Open => "open".to_string(),
            Self::Send => "send".to_string(),
            Self::Next => "next".to_string(),
            Self::Finish => "finish".to_string(),
            Self::Abort => "abort".to_string(),
            Self::Unknown(value) => value.clone(),
        }
    }
}

#[async_trait]
pub trait ProvenanceContextReader: Send + Sync {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>>;

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;
}
