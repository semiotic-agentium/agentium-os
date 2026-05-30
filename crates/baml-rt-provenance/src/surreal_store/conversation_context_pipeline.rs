// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typestate pipeline for assembling agent-visible conversation context from graph rows.
//!
//! ## Stages
//!
//! - [`Raw`]: Rows from the Surreal query; session-step replay fields are not yet hydrated.
//! - [`Hydrated`]: `SendDone` replay payloads and `Read` **raw** archive line lists are filled from
//!   the payload store and prior steps in the batch.
//! - [`Canonical`]: **Identity** transform — the graph rows passed through [`Hydrated`] are kept as-is.
//!   (Older versions removed `ToolCall` / `ToolResult` rows when `SendDone` listed
//!   `informed_by`; that **read-time** suppression is invalid for provenance truth: the read path
//!   must not hide stored events.)
//!
//! Only [`ConversationContextBatch<Canonical>`] exposes [`ConversationContextBatch::into_items`]
//! for consumers that must not accidentally project pre-hydration rows.
//!
//! Rendering is stateless in [`baml_rt_tools::prompt_projection`] (no cross-item “dedup” of archive
//! views). If a duplicate row should not exist, fix the write path; do not collapse at read time.

use std::marker::PhantomData;

use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceConversationContextItem, SessionStepOp,
};
use baml_rt_tools::archive_read::render_to_lines;

use super::SurrealProvenanceStore;
use crate::error::Result;

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Raw {}
    impl Sealed for super::Hydrated {}
    impl Sealed for super::Canonical {}
}

/// Graph-assembled rows; session steps may still need payload hydration.
#[derive(Debug, Clone, Copy)]
pub struct Raw;

/// Session-step `SendDone` and `Read` replay derived fields populated.
#[derive(Debug, Clone, Copy)]
pub struct Hydrated;

/// Ready for prompt projection: post-hydration, **no** row drops.
#[derive(Debug, Clone, Copy)]
pub struct Canonical;

/// Batch of conversation items at pipeline stage `S`.
pub struct ConversationContextBatch<S: sealed::Sealed> {
    items: Vec<ProvenanceConversationContextItem>,
    _stage: PhantomData<S>,
}

impl ConversationContextBatch<Raw> {
    pub(crate) fn from_graph_rows(items: Vec<ProvenanceConversationContextItem>) -> Self {
        Self {
            items,
            _stage: PhantomData,
        }
    }

    pub(crate) async fn hydrate(
        mut self,
        store: &SurrealProvenanceStore,
    ) -> Result<ConversationContextBatch<Hydrated>> {
        store
            .hydrate_session_step_send_done_payloads(&mut self.items)
            .await?;
        hydrate_session_step_read_replays(&mut self.items);
        Ok(ConversationContextBatch {
            items: self.items,
            _stage: PhantomData,
        })
    }
}

impl ConversationContextBatch<Hydrated> {
    /// Historical name: previously removed tool rows when `SendDone` covered their anchors.
    /// That **must not** run on the read path — this is now a no-op.
    pub(crate) fn canonicalize_suppress_covered_tool_rows(
        self,
    ) -> ConversationContextBatch<Canonical> {
        ConversationContextBatch {
            items: self.items,
            _stage: PhantomData,
        }
    }

    #[cfg(test)]
    fn from_items_hydrated_for_test(items: Vec<ProvenanceConversationContextItem>) -> Self {
        Self {
            items,
            _stage: PhantomData,
        }
    }
}

impl ConversationContextBatch<Canonical> {
    pub(crate) fn into_items(self) -> Vec<ProvenanceConversationContextItem> {
        self.items
    }
}

/// Derives [`crate::store::SessionStepContent::read_replay_lines`] for archive read ops from an earlier
/// `SendDone`’s hydrated replay payload in the same conversation batch. Pure derivation (no I/O).
///
/// Each line is a **raw rendered archive line** (the same as [`ArchiveEntry::content`] / `render_to_lines`),
/// not a pre-formatted `cat -n` / `grep -n` session read block. Prompt projection and episode re-apply
/// [`baml_rt_tools::archive_read::format_session_read_body_from_rendered`].
fn hydrate_session_step_read_replays(items: &mut [ProvenanceConversationContextItem]) {
    for i in 0..items.len() {
        let archive_ref = {
            let ConversationItemContent::SessionStep(ss) = &items[i].content else {
                continue;
            };
            match &ss.op {
                SessionStepOp::SearchRead { archive_ref, .. } => archive_ref.clone(),
                SessionStepOp::PageRead { archive_ref, .. } => archive_ref.clone(),
                _ => continue,
            }
        };

        let mut base: Option<serde_json::Value> = None;
        for j in (0..i).rev() {
            let ConversationItemContent::SessionStep(ss) = &items[j].content else {
                continue;
            };
            let SessionStepOp::SendDone {
                archive_ref: ar, ..
            } = &ss.op
            else {
                continue;
            };
            if ar == &archive_ref {
                base = ss.send_done_replay_payload.clone();
                break;
            }
        }
        let Some(val) = base else {
            continue;
        };
        let rendered = render_to_lines(&val);
        if rendered.is_empty() {
            continue;
        }
        let lines: Vec<String> = rendered.lines().map(str::to_string).collect();
        if lines.is_empty() {
            continue;
        }
        let ConversationItemContent::SessionStep(ss) = &mut items[i].content else {
            continue;
        };
        ss.read_replay_lines = Some(lines);
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_conversation::view::{
        ConversationItemContent, ProvenanceConversationContextItem, SessionStepContent,
        SessionStepOp, ToolCallContent, ToolOutcome, ToolResultContent, ToolSessionPhase,
    };
    use baml_rt_core::ids::ActivityAnchorId;
    use serde_json::json;

    #[test]
    fn read_path_keeps_tool_rows_even_when_send_done_informs() {
        let anchor = ActivityAnchorId::from("sess-tool-anchor");
        let items = vec![
            ProvenanceConversationContextItem {
                timestamp_ms: 1,
                activity_anchor: anchor.clone(),
                role: "tool".into(),
                content: ConversationItemContent::ToolCall(ToolCallContent {
                    tool_name: "t".into(),
                    args: json!({}),
                    fsm_phase: ToolSessionPhase::Send,
                }),
            },
            ProvenanceConversationContextItem {
                timestamp_ms: 2,
                activity_anchor: anchor.clone(),
                role: "tool".into(),
                content: ConversationItemContent::ToolResult(ToolResultContent {
                    tool_name: "t".into(),
                    fsm_phase: ToolSessionPhase::Send,
                    outcome: ToolOutcome::Result(json!("body")),
                }),
            },
            ProvenanceConversationContextItem {
                timestamp_ms: 3,
                activity_anchor: ActivityAnchorId::from("step-node"),
                role: "tool".into(),
                content: ConversationItemContent::SessionStep(SessionStepContent {
                    tool_name: "t".into(),
                    op: SessionStepOp::SendDone {
                        archive_ref: "@1".into(),
                        header: "h".into(),
                        informed_by: anchor.as_str().to_string(),
                    },
                    send_done_replay_payload: None,
                    read_replay_lines: None,
                }),
            },
        ];
        let out = super::ConversationContextBatch::from_items_hydrated_for_test(items)
            .canonicalize_suppress_covered_tool_rows();
        let out = out.into_items();
        assert_eq!(out.len(), 3, "all stored rows surface for the LLM");
    }

    #[test]
    fn canonical_preserves_tool_rows_when_no_send_done_informed_by() {
        let anchor = ActivityAnchorId::from("execute-only");
        let items = vec![
            ProvenanceConversationContextItem {
                timestamp_ms: 1,
                activity_anchor: anchor.clone(),
                role: "tool".into(),
                content: ConversationItemContent::ToolCall(ToolCallContent {
                    tool_name: "t".into(),
                    args: json!({}),
                    fsm_phase: ToolSessionPhase::Execute,
                }),
            },
            ProvenanceConversationContextItem {
                timestamp_ms: 2,
                activity_anchor: anchor.clone(),
                role: "tool".into(),
                content: ConversationItemContent::ToolResult(ToolResultContent {
                    tool_name: "t".into(),
                    fsm_phase: ToolSessionPhase::Execute,
                    outcome: ToolOutcome::Result(json!(5)),
                }),
            },
        ];
        let out = super::ConversationContextBatch::from_items_hydrated_for_test(items)
            .canonicalize_suppress_covered_tool_rows();
        let out = out.into_items();
        assert_eq!(out.len(), 2);
    }
}
