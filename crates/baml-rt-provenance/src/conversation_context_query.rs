//! Defaults for [`ProvenanceQueryApi::query_conversation_context`]-style APIs.
//!
//! Call sites that do not pass an explicit `limit` (for example
//! `Arc<dyn ProvenanceContextReader>::conversation_context` with `None`) may still apply a
//! default cap at a higher layer (e.g. A2A prompt building). The numeric policy lives here,
//! not in transport.
//!
//! **Write path:** every material `SessionStep` / tool row should be recorded in the graph; the
//! read path must not drop or merge rows to “fix” redundant emits.

/// Default maximum number of [`baml_rt_conversation::ProvenanceConversationContextItem`] rows to
/// fetch when assembling LLM-visible `conversation_history` (A2A `ProjectingConversationContextProvider`
/// and similar). Truncation at this cap is recorded with a debug log in the store reader.
pub const DEFAULT_LLM_CONTEXT_ITEM_CAP: usize = 40;
