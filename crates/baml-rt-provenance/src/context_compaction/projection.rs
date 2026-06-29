// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Apply compaction heads to conversation item streams for agent-facing projection.

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::ids::ActivityAnchorId;

use super::{range::item_is_live_planning_obligation, types::ContextCompactionHead};
use crate::read::transcript::TranscriptProjectionProfile;

/// Synthetic compaction summary row for agent prompt projection.
#[derive(Debug, Clone)]
pub struct CompactionSummaryItem {
    pub activity_anchor: ActivityAnchorId,
    pub timestamp_ms: u64,
    pub summary_text: String,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
}

impl CompactionSummaryItem {
    pub fn to_conversation_item(&self) -> ProvenanceConversationContextItem {
        ProvenanceConversationContextItem {
            timestamp_ms: self.timestamp_ms,
            activity_anchor: self.activity_anchor.clone(),
            role: "system".to_string(),
            content: ConversationItemContent::CompactionSummary {
                summary: self.summary_text.clone(),
                covered_event_order_start: self.covered_event_order_start,
                covered_event_order_end: self.covered_event_order_end,
            },
            user_speaker_kind: None,
        }
    }
}

/// Apply profile-specific compaction semantics to a chronologically ordered item list.
#[must_use]
pub fn apply_compaction_profile(
    profile: TranscriptProjectionProfile,
    items: Vec<ProvenanceConversationContextItem>,
    head: Option<&ContextCompactionHead>,
) -> Vec<ProvenanceConversationContextItem> {
    match profile {
        TranscriptProjectionProfile::ReplayFull | TranscriptProjectionProfile::CompactionAudit => {
            items
        }
        TranscriptProjectionProfile::OperatorTimeline => enrich_operator_timeline(items, head),
        TranscriptProjectionProfile::AgentPromptIndex
        | TranscriptProjectionProfile::AgentPromptCompacted
        | TranscriptProjectionProfile::LiveStructuralDelta => {
            if matches!(profile, TranscriptProjectionProfile::AgentPromptCompacted) {
                apply_agent_prompt_compaction(items, head)
            } else {
                items
            }
        }
    }
}

fn apply_agent_prompt_compaction(
    items: Vec<ProvenanceConversationContextItem>,
    head: Option<&ContextCompactionHead>,
) -> Vec<ProvenanceConversationContextItem> {
    let Some(head) = head else {
        return items;
    };

    let tail: Vec<_> = items
        .into_iter()
        .filter(|item| {
            item.timestamp_ms > head.covered_event_order_end
                || item_is_live_planning_obligation(item)
        })
        .collect();

    let mut out = Vec::with_capacity(tail.len() + 1);
    out.push(
        CompactionSummaryItem {
            activity_anchor: head.activity_anchor.clone(),
            timestamp_ms: head.event_order,
            summary_text: head.summary_text.clone(),
            covered_event_order_start: head.covered_event_order_start,
            covered_event_order_end: head.covered_event_order_end,
        }
        .to_conversation_item(),
    );
    out.extend(tail);
    out
}

fn enrich_operator_timeline(
    items: Vec<ProvenanceConversationContextItem>,
    head: Option<&ContextCompactionHead>,
) -> Vec<ProvenanceConversationContextItem> {
    let Some(head) = head else {
        return items;
    };
    let marker = ProvenanceConversationContextItem {
        timestamp_ms: head.event_order,
        activity_anchor: head.activity_anchor.clone(),
        role: "system".to_string(),
        content: ConversationItemContent::CompactionSummary {
            summary: format!(
                "[compaction marker] covered {}..{} ({} chars)",
                head.covered_event_order_start,
                head.covered_event_order_end,
                head.summary_text.len()
            ),
            covered_event_order_start: head.covered_event_order_start,
            covered_event_order_end: head.covered_event_order_end,
        },
        user_speaker_kind: None,
    };
    let mut out = Vec::with_capacity(items.len() + 1);
    out.push(marker);
    out.extend(items);
    out
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::ActivityAnchorId;

    use super::*;

    fn msg(order: u64, text: &str) -> ProvenanceConversationContextItem {
        ProvenanceConversationContextItem {
            timestamp_ms: order,
            activity_anchor: ActivityAnchorId::from(format!("a{order}")),
            role: "user".into(),
            content: ConversationItemContent::Message {
                text: text.into(),
                citations: vec![],
            },
            user_speaker_kind: None,
        }
    }

    #[test]
    fn agent_compacted_keeps_tail_and_summary() {
        let items: Vec<_> = (1..=5).map(|i| msg(i * 10, &format!("m{i}"))).collect();
        let head = ContextCompactionHead {
            activity_anchor: ActivityAnchorId::from("compact-1"),
            covered_event_order_start: 10,
            covered_event_order_end: 30,
            summary_text: "earlier work summarized".into(),
            trigger: super::super::types::ContextCompactionTrigger::PostTurnThreshold,
            event_order: 35,
            task_entity_id: None,
        };
        let out = apply_compaction_profile(
            TranscriptProjectionProfile::AgentPromptCompacted,
            items,
            Some(&head),
        );
        assert_eq!(out.len(), 3);
        assert!(matches!(
            out[0].content,
            ConversationItemContent::CompactionSummary { .. }
        ));
        assert_eq!(out[1].timestamp_ms, 40);
        assert_eq!(out[2].timestamp_ms, 50);
    }
}
