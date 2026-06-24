// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Select a sealed compactable prefix from conversation rows.
use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};

use super::types::ContextCompactionPolicy;

/// Selected prefix eligible for compaction plus retained recent tail boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactableRange {
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub recent_tail_start_event_order: u64,
}

/// Returns `None` when compaction should not run (too few rows, etc.).
#[must_use]
pub fn select_compactable_range(
    items: &[ProvenanceConversationContextItem],
    policy: &ContextCompactionPolicy,
) -> Option<CompactableRange> {
    if items.len() < policy.item_threshold {
        return None;
    }

    let tail_keep = policy.recent_tail_retention.max(1);
    if items.len() <= tail_keep {
        return None;
    }

    let split_at = items.len().saturating_sub(tail_keep);
    let prefix_rows = &items[..split_at];
    let tail_rows = &items[split_at..];

    if prefix_rows.is_empty() {
        return None;
    }

    let tail_start = tail_rows
        .first()
        .map(|r| r.timestamp_ms)
        .unwrap_or(u64::MAX);

    let covered_start = prefix_rows.first().map(|r| r.timestamp_ms).unwrap_or(0);
    let covered_end = prefix_rows
        .last()
        .map(|r| r.timestamp_ms)
        .unwrap_or(covered_start);

    Some(CompactableRange {
        covered_event_order_start: covered_start,
        covered_event_order_end: covered_end,
        recent_tail_start_event_order: tail_start,
    })
}

/// True when any item in the slice is live planning state that must not be summarized away.
#[must_use]
pub fn item_is_live_planning_obligation(item: &ProvenanceConversationContextItem) -> bool {
    let ConversationItemContent::Planning(plan) = &item.content else {
        return false;
    };
    matches!(
        plan.kind,
        baml_rt_conversation::planning::PlanningEventKind::IntentResolved
            | baml_rt_conversation::planning::PlanningEventKind::PlanCommitted
            | baml_rt_conversation::planning::PlanningEventKind::PlanStepStatusChanged
    ) && plan
        .new_status
        .as_deref()
        .is_some_and(|s| s != "completed" && s != "aborted")
        || matches!(
            plan.kind,
            baml_rt_conversation::planning::PlanningEventKind::IntentResolved
                | baml_rt_conversation::planning::PlanningEventKind::PlanCommitted
        )
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::ActivityAnchorId;

    use super::*;

    fn dummy_items(count: usize) -> Vec<ProvenanceConversationContextItem> {
        (0..count)
            .map(|i| ProvenanceConversationContextItem {
                timestamp_ms: (i as u64 + 1) * 10,
                activity_anchor: ActivityAnchorId::from(format!("a{i}")),
                role: "user".into(),
                content: ConversationItemContent::Message {
                    text: format!("msg {i}"),
                    citations: vec![],
                },
                user_speaker_kind: None,
            })
            .collect()
    }

    #[test]
    fn selects_prefix_leaving_recent_tail() {
        let items = dummy_items(50);
        let policy = ContextCompactionPolicy {
            item_threshold: 40,
            recent_tail_retention: 8,
            ..Default::default()
        };
        let range = select_compactable_range(&items, &policy).expect("range");
        assert_eq!(range.covered_event_order_end, 420);
        assert_eq!(range.recent_tail_start_event_order, 430);
    }

    #[test]
    fn skips_when_below_item_threshold() {
        let items = dummy_items(5);
        assert!(select_compactable_range(&items, &ContextCompactionPolicy::default()).is_none());
    }

    #[test]
    fn live_planning_obligation_detected() {
        use baml_rt_conversation::planning::{PlanningEventContent, PlanningEventKind};
        let item = ProvenanceConversationContextItem {
            timestamp_ms: 1,
            activity_anchor: ActivityAnchorId::from("a1"),
            role: "system".into(),
            content: ConversationItemContent::Planning(PlanningEventContent {
                kind: PlanningEventKind::PlanStepStatusChanged,
                summary: "step".into(),
                detail: None,
                intent_id: None,
                plan_id: Some("p1".into()),
                step_id: Some("s1".into()),
                old_status: None,
                new_status: Some("in_progress".into()),
            }),
            user_speaker_kind: None,
        };
        assert!(item_is_live_planning_obligation(&item));
    }
}
