// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Payload for notifying subscribers when provenance rows affecting a conversation context
//! have been committed (so transcript reads are consistent).

/// Broadcast after a successful [`baml_rt_provenance::ProvenanceWriter::add_event`] for any
/// context-scoped event. Used by the operator UI to refresh `/conversation-history` streams **after**
/// graph writes land (A2A task updates alone are not a transcript-consistency boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHistoryUpdate {
    pub context_id: String,
    pub task_id: Option<String>,
}
