// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Partition conversation items into compactable prefix vs retained tail.

use baml_rt_conversation::view::ProvenanceConversationContextItem;

use super::range::{CompactableRange, item_is_live_planning_obligation};

/// Split items using the same retention rules as agent prompt compaction on read.
#[must_use]
pub fn partition_items_for_compaction(
    items: &[ProvenanceConversationContextItem],
    range: &CompactableRange,
) -> (
    Vec<ProvenanceConversationContextItem>,
    Vec<ProvenanceConversationContextItem>,
) {
    let prefix: Vec<_> = items
        .iter()
        .filter(|item| {
            item.timestamp_ms <= range.covered_event_order_end
                && !item_is_live_planning_obligation(item)
        })
        .cloned()
        .collect();
    let tail: Vec<_> = items
        .iter()
        .filter(|item| {
            item.timestamp_ms > range.covered_event_order_end
                || item_is_live_planning_obligation(item)
        })
        .cloned()
        .collect();
    (prefix, tail)
}

#[cfg(test)]
mod tests {
    use baml_rt_conversation::{
        planning::{PlanningEventContent, PlanningEventKind},
        view::{ConversationItemContent, ProvenanceConversationContextItem},
    };
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

    fn live_plan(order: u64) -> ProvenanceConversationContextItem {
        ProvenanceConversationContextItem {
            timestamp_ms: order,
            activity_anchor: ActivityAnchorId::from(format!("p{order}")),
            role: "system".into(),
            content: ConversationItemContent::Planning(PlanningEventContent {
                kind: PlanningEventKind::PlanStepStatusChanged,
                summary: "deploy".into(),
                detail: None,
                intent_id: None,
                plan_id: Some("p1".into()),
                step_id: Some("s1".into()),
                old_status: None,
                new_status: Some("in_progress".into()),
            }),
            user_speaker_kind: None,
        }
    }

    #[test]
    fn live_planning_stays_in_tail_not_prefix() {
        let range = CompactableRange {
            covered_event_order_start: 10,
            covered_event_order_end: 50,
            recent_tail_start_event_order: 40,
        };
        let items = vec![msg(10, "old"), live_plan(20), msg(60, "recent")];
        let (prefix, tail) = partition_items_for_compaction(&items, &range);
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0].timestamp_ms, 10);
        assert_eq!(tail.len(), 2);
        assert!(
            tail.iter()
                .any(|i| matches!(i.content, ConversationItemContent::Planning { .. }))
        );
    }
}
