// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host-owned context compaction: range selection, projection, compactor, and hooks.

pub mod compactor;
pub mod projection;
pub mod range;
pub mod subscriber;
pub mod types;

pub use compactor::{
    ContextCompactionService, build_compaction_summary, validate_summary_preserves_archive_refs,
};
pub use projection::{CompactionSummaryItem, apply_compaction_profile};
pub use range::{CompactableRange, TranscriptIndexRow, select_compactable_range};
pub use subscriber::ContextCompactionSubscriber;
pub use types::{
    ContextCompactionHead, ContextCompactionPolicy, ContextCompactionRecord,
    ContextCompactionTrigger,
};

use crate::conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP;

/// Post-turn compaction may run when item count reaches this threshold.
pub const DEFAULT_COMPACTION_ITEM_THRESHOLD: usize = DEFAULT_LLM_CONTEXT_ITEM_CAP;

/// Pre-model emergency compaction when serialized prompt bytes exceed this budget.
pub const DEFAULT_COMPACTION_PROMPT_BYTES_THRESHOLD: u64 = 32_768;

/// Rows kept verbatim at the end of the transcript after compaction.
pub const DEFAULT_RECENT_TAIL_RETENTION: usize = 12;
