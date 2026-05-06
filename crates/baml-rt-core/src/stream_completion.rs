//! Explicit stream completion semantics for A2A yield-based streams.
//!
//! Completion is no longer inferred from quiescence; it is carried as a
//! first-class reason so transport and tests can enforce invariants.
//!
//! **Wire vs semantic:** [`StreamCompletion::InputRequired`] means the task is suspended for user
//! input. It is **not** wire-final — formatters must keep `final: false` on stream chunks so clients
//! can show `TASK_STATE_INPUT_REQUIRED` without treating the HTTP/SSE body as terminated. Use
//! [`StreamCompletion::is_wire_final`] to decide `final: true` on the JSON-RPC envelope.

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

impl StreamCompletion {
    /// True iff the client should receive `final: true` for this completion (stream ended for wire).
    pub fn is_wire_final(self) -> bool {
        matches!(
            self,
            StreamCompletion::SemanticFinal
                | StreamCompletion::ChannelClosed
                | StreamCompletion::Timeout
        )
    }
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
