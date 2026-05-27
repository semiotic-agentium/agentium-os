//! Bounded transcript reads via `context_transcript_index`.

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::ids::{ContextId, TaskId};

use crate::error::Result;

/// Slice request: ordered rows after `after_event_order`, capped at `limit`.
#[derive(Debug, Clone)]
pub struct TranscriptSliceSpec {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub agent_package: Option<String>,
    pub after_event_order: u64,
    pub limit: usize,
    /// When false, planning/operational extension rows are omitted (conversation-history default).
    pub include_extensions: bool,
}

#[derive(Debug, Clone)]
pub struct TranscriptSlice {
    pub items: Vec<ProvenanceConversationContextItem>,
    pub max_event_order: u64,
    /// Last row `event_order` in this slice when more rows may exist.
    pub next_after_event_order: Option<u64>,
}

#[async_trait]
pub trait TranscriptReader: Send + Sync {
    async fn slice(&self, spec: TranscriptSliceSpec) -> Result<TranscriptSlice>;
}
