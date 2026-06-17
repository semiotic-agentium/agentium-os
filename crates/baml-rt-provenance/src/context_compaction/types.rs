// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Types for host-owned context/history compaction provenance events.

use baml_rt_core::ids::{ActivityAnchorId, ContextId, TaskId};
use serde::{Deserialize, Serialize};

pub use crate::events::ContextCompactionTrigger;

/// Latest compaction head for a context (optionally task-scoped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionHead {
    pub activity_anchor: ActivityAnchorId,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub summary_text: String,
    pub trigger: ContextCompactionTrigger,
    pub event_order: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_entity_id: Option<String>,
}

/// Policy knobs for when to compact.
#[derive(Debug, Clone, Copy)]
pub struct ContextCompactionPolicy {
    /// Item count at or above which post-turn compaction may run.
    pub item_threshold: usize,
    /// Serialized prompt bytes at or above which pre-model emergency may run.
    pub prompt_bytes_threshold: u64,
    /// Recent tail rows kept verbatim after compaction.
    pub recent_tail_retention: usize,
}

impl Default for ContextCompactionPolicy {
    fn default() -> Self {
        Self {
            item_threshold: super::DEFAULT_COMPACTION_ITEM_THRESHOLD,
            prompt_bytes_threshold: super::DEFAULT_COMPACTION_PROMPT_BYTES_THRESHOLD,
            recent_tail_retention: super::DEFAULT_RECENT_TAIL_RETENTION,
        }
    }
}

/// Inputs collected before writing a compaction provenance event.
#[derive(Debug, Clone)]
pub struct ContextCompactionRecord {
    pub context_id: ContextId,
    pub task_id: Option<TaskId>,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub covered_node_ids: Vec<String>,
    pub summary_text: String,
    pub trigger: ContextCompactionTrigger,
    pub recent_tail_retention: usize,
    pub pre_row_count: u64,
    pub post_row_count: u64,
    pub pre_prompt_bytes: u64,
    pub post_prompt_bytes: u64,
    pub source_render_hash: String,
    pub excluded_unresolved: bool,
}
