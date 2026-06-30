// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Types for host-owned context/history compaction provenance events.

use std::sync::Arc;

use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, TaskId};
use baml_rt_tools::archive_refs::RefTable;
use serde::{Deserialize, Serialize};

pub use crate::events::ContextCompactionTrigger;

/// Identity for a compaction attempt (routing + provenance scope).
#[derive(Debug, Clone)]
pub struct CompactionRequest {
    pub context_id: ContextId,
    pub agent_id: AgentId,
}

/// Prepared prefix handed to any summarizer backend.
#[derive(Debug, Clone)]
pub struct CompactionPrefixInput {
    /// Rendered transcript of the sealed prefix (LLM input).
    pub source_rendered: String,
    pub active_planning_digest: Option<String>,
    pub recent_tail_preview: Option<String>,
    /// Hydrated ref table for validating wire refs cited in the summary.
    pub ref_table: Arc<RefTable>,
}

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
