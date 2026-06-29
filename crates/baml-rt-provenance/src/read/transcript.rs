// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Bounded transcript reads via `context_transcript_index`.

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;

use crate::{
    error::Result,
    observation::{EventOrder, ObservationScope},
};

/// How a transcript page is projected for downstream consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscriptProjectionProfile {
    /// Operator UI: index rows + operational/planning graph extensions.
    #[default]
    OperatorTimeline,
    /// Agent prompt assembly: index rows only (no extension enrichment).
    AgentPromptIndex,
    /// Agent prompt with latest compaction summary + recent tail.
    AgentPromptCompacted,
    /// Live SSE delta tail: index rows only.
    LiveStructuralDelta,
    /// Full chronological history ignoring compaction boundaries.
    ReplayFull,
    /// Compaction audit rows (summary + covered range metadata).
    CompactionAudit,
}

impl TranscriptProjectionProfile {
    #[must_use]
    pub const fn enrich_from_graph_extensions(self) -> bool {
        matches!(self, Self::OperatorTimeline)
    }

    #[must_use]
    pub const fn uses_compaction(self) -> bool {
        matches!(self, Self::AgentPromptCompacted)
    }
}

/// Bounded transcript page request — sole input to [`TranscriptEngine`].
#[derive(Debug, Clone)]
pub struct TranscriptPageRequest {
    pub scope: ObservationScope,
    pub limit: usize,
    pub profile: TranscriptProjectionProfile,
}

impl TranscriptPageRequest {
    /// Exclusive lower bound; `None` when reading from the start of the timeline.
    #[must_use]
    pub fn after_event_order_exclusive(&self) -> Option<u64> {
        self.scope
            .temporal
            .after_event_order()
            .map(EventOrder::as_u64)
    }
}

/// When task-scoped index lookup returns no rows, the engine widens to context-wide index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscriptScopeWidening {
    #[default]
    None,
    ContextFallback,
}

#[derive(Debug, Clone)]
pub struct TranscriptPage {
    pub scope: ObservationScope,
    pub items: Vec<ProvenanceConversationContextItem>,
    pub max_event_order: u64,
    pub next_after_event_order: Option<u64>,
    pub scope_widening: TranscriptScopeWidening,
}

/// Index-backed transcript reads — single authority for operator timelines.
#[async_trait]
pub trait TranscriptEngine: Send + Sync {
    async fn page(&self, request: TranscriptPageRequest) -> Result<TranscriptPage>;
}
