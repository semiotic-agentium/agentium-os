//! Typestate pipeline for assembling agent-visible conversation context from graph rows.
//!
//! ## Stages
//!
//! - [`Raw`]: Rows from the Surreal query; session-step replay fields are not yet hydrated.
//! - [`Hydrated`]: `SendDone` replay payloads and `Read` replay lines are filled from the payload
//!   store and prior steps in the batch.
//! - [`Canonical`]: Raw [`crate::store::ConversationItemContent::ToolCall`] /
//!   [`crate::store::ConversationItemContent::ToolResult`] rows are removed when a
//!   [`crate::store::SessionStepOp::SendDone`] covers the same tool activity via
//!   `informed_by_tool_activity_anchor`, so prompt projection sees at most one expanded body per
//!   logical send.
//!
//! Only [`ConversationContextBatch<Canonical>`] exposes [`ConversationContextBatch::into_items`]
//! for consumers that must not accidentally project pre-canonical rows.
//!
//! Within-batch `@N` dedup for rendering is implemented in [`baml_rt_tools::prompt_projection`];
//! row-level suppression of duplicate tool rows is this pipeline’s responsibility, not the
//! projector’s.

use std::{collections::HashSet, marker::PhantomData};

use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceConversationContextItem, SessionStepOp,
};
use baml_rt_tools::archive_read::SESSION_HISTORY_READ_REPLAY_MAX_LINES;

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

/// Ready for prompt projection: duplicate raw tool rows for covered sends removed.
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
    pub(crate) fn canonicalize_suppress_covered_tool_rows(
        self,
    ) -> ConversationContextBatch<Canonical> {
        let items = suppress_tool_rows_covered_by_session_send_done(self.items);
        ConversationContextBatch {
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
fn hydrate_session_step_read_replays(items: &mut [ProvenanceConversationContextItem]) {
    for i in 0..items.len() {
        let (archive_ref, grep_opt, offset, limit) = {
            let ConversationItemContent::SessionStep(ss) = &items[i].content else {
                continue;
            };
            match &ss.op {
                SessionStepOp::SearchRead {
                    archive_ref,
                    grep,
                    offset,
                    limit,
                } => (archive_ref.clone(), Some(grep.clone()), *offset, *limit),
                SessionStepOp::PageRead {
                    archive_ref,
                    offset,
                    limit,
                } => (archive_ref.clone(), None, *offset, *limit),
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
        let page_limit = limit.clamp(1, SESSION_HISTORY_READ_REPLAY_MAX_LINES);
        let Some(body) = baml_rt_tools::archive_read::format_session_read_body_from_json_value(
            &val,
            archive_ref.as_str(),
            grep_opt.as_deref(),
            offset,
            baml_rt_tools::archive_read::PageLimit::new(page_limit),
        ) else {
            continue;
        };
        let lines: Vec<String> = body.lines().map(str::to_string).collect();
        if lines.is_empty() {
            continue;
        }
        let ConversationItemContent::SessionStep(ss) = &mut items[i].content else {
            continue;
        };
        ss.read_replay_lines = Some(lines);
    }
}

/// Removes [`ConversationItemContent::ToolCall`] / [`ConversationItemContent::ToolResult`] when a
/// [`SessionStepOp::SendDone`] in the same batch lists that tool activity anchor in `informed_by`.
fn suppress_tool_rows_covered_by_session_send_done(
    items: Vec<ProvenanceConversationContextItem>,
) -> Vec<ProvenanceConversationContextItem> {
    let mut covered: HashSet<String> = HashSet::new();
    for item in &items {
        let ConversationItemContent::SessionStep(ss) = &item.content else {
            continue;
        };
        let SessionStepOp::SendDone { informed_by, .. } = &ss.op else {
            continue;
        };
        if !informed_by.is_empty() {
            covered.insert(informed_by.clone());
        }
    }
    if covered.is_empty() {
        return items;
    }
    items
        .into_iter()
        .filter(|item| match &item.content {
            ConversationItemContent::ToolCall(_) | ConversationItemContent::ToolResult(_) => {
                !covered.contains(item.activity_anchor.as_str())
            }
            _ => true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use baml_rt_conversation::view::{
        ConversationItemContent, ProvenanceConversationContextItem, SessionStepContent,
        SessionStepOp, ToolCallContent, ToolOutcome, ToolResultContent, ToolSessionPhase,
    };
    use baml_rt_core::ids::ActivityAnchorId;
    use serde_json::json;

    use super::suppress_tool_rows_covered_by_session_send_done;

    #[test]
    fn canonical_removes_tool_rows_when_send_done_informs() {
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
        let out = suppress_tool_rows_covered_by_session_send_done(items);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0].content,
            ConversationItemContent::SessionStep(_)
        ));
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
        let out = suppress_tool_rows_covered_by_session_send_done(items);
        assert_eq!(out.len(), 2);
    }
}
