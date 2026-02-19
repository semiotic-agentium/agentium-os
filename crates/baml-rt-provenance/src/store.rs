//! Provenance store traits: write events and read task/conversation context.
//!
//! ## Read intents: agent context vs API
//!
//! Two query paths are distinguished at the type level so the type system enforces the
//! intended behavior:
//!
//! - **[ProvenanceReadIntent::AgentContext]** (via [ProvenanceContextReader]): Used by the agent
//!   runtime for task and conversation context when building prompts. **No-stale-read invariant**
//!   applies: a read must reflect all prior completed writes. Implementations must enforce this
//!   (e.g. serialized worker so reads see prior writes).
//! - **[ProvenanceReadIntent::Api]** (via [ProvenanceQueryApi]): Exposed to APIs for display,
//!   analytics, or ad-hoc queries. **No guarantee** of no-stale-read; implementations may use
//!   read replicas, caches, or relaxed ordering. Other provenance queries do not require
//!   consistency.
//!
//! The **typed enum** [ProvenanceReadIntent] documents these two behaviors; the **two traits**
//! enforce at the type level: agent code holds [ProvenanceContextReader] (or [ProvenanceWriter]),
//! API code holds [ProvenanceQueryApi]. The same store can implement both.
//!
//! ## No-stale-read invariant (ProvenanceContextReader only)
//!
//! - **Property:** ∀ write W completed before read R via [ProvenanceContextReader]: R reflects W.
//! - **Enforcement:** Implementations use a single serialized worker for writes and reads so that
//!   any read that starts after a write completes sees that write. Callers must await
//!   [ProvenanceWriter::add_event] (or [ProvenanceWriter::add_events]) before calling the reader
//!   methods if they need to see those events.

use async_trait::async_trait;
use baml_rt_core::ids::ContextId;
use serde_json::Value;

use crate::{error::Result, events::ProvEvent};

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

/// Intent for a provenance read: enforces which guarantee the caller gets.
///
/// - **AgentContext:** No-stale-read required. Use via [ProvenanceContextReader]; implementations
///   must ensure reads reflect all prior completed writes (e.g. serialized with writes).
/// - **Api:** No consistency guarantee. Use via [ProvenanceQueryApi]; for APIs, display, analytics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvenanceReadIntent {
    /// Agent/task/conversation context: read must reflect all prior completed writes.
    AgentContext,
    /// API or analytics: no guarantee of no-stale-read.
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSessionPhase {
    Execute,
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
            "execute" => Self::Execute,
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
            Self::Execute => "execute".to_string(),
            Self::Open => "open".to_string(),
            Self::Send => "send".to_string(),
            Self::Next => "next".to_string(),
            Self::Finish => "finish".to_string(),
            Self::Abort => "abort".to_string(),
            Self::Unknown(value) => value.clone(),
        }
    }
}

/// Reader for task and conversation context used by agents to build prompts.
///
/// **No-stale-read invariant:** A read of [context_messages] or [conversation_context] must
/// reflect all prior writes that completed before the read. This trait corresponds to
/// [ProvenanceReadIntent::AgentContext]. Use [ProvenanceQueryApi] for API-exposed reads that do
/// not require this guarantee.
#[async_trait]
pub trait ProvenanceContextReader: Send + Sync {
    /// Messages for the given context (user + assistant). Used for conversation history.
    /// Must reflect all prior [ProvenanceWriter::add_event] calls that completed before this call.
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>>;

    /// Full conversation context (messages + tool calls) for the given context. Used for
    /// BAML conversation context. Must reflect all prior [ProvenanceWriter::add_event] calls
    /// that completed before this call.
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;
}

/// Query API for provenance context: **does not** guarantee no-stale-read.
///
/// Use this trait for API-exposed reads (display, analytics, ad-hoc queries). Implementations
/// may use read replicas, caches, or relaxed ordering. Corresponds to [ProvenanceReadIntent::Api].
/// For agent/task context that requires no-stale-read, use [ProvenanceContextReader] instead.
#[async_trait]
pub trait ProvenanceQueryApi: Send + Sync {
    /// Messages for the given context. No guarantee that the result reflects the latest writes.
    async fn query_context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>>;

    /// Full conversation context for the given context. No guarantee of consistency with writes.
    async fn query_conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>>;
}
