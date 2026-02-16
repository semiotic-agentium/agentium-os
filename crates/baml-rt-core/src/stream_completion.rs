//! Explicit stream completion semantics for A2A yield-based streams.
//!
//! Completion is no longer inferred from quiescence; it is carried as a
//! first-class reason so transport and tests can enforce invariants.

use serde_json::Value;

/// Why the stream collection stopped. Eliminates implicit heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCompletion {
    /// Agent yielded a chunk with TASK_STATE_COMPLETED or TASK_STATE_FAILED.
    SemanticFinal,
    /// Agent yielded TASK_STATE_INPUT_REQUIRED and is awaiting next user message.
    InputRequired,
    /// Producer closed the channel (sender dropped).
    ChannelClosed,
    /// Safety timeout; stream may be truncated.
    Timeout,
}

/// Result of stream collection with explicit completion semantics.
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub chunks: Vec<Value>,
    pub completion: StreamCompletion,
}

impl StreamResult {
    /// True iff stream ended with semantic or channel-closed finality.
    pub fn is_semantically_final(&self) -> bool {
        matches!(
            self.completion,
            StreamCompletion::SemanticFinal | StreamCompletion::ChannelClosed
        )
    }
}
