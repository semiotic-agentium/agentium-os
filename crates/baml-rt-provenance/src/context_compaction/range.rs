// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Select a sealed compactable prefix from transcript rows.

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};

use super::types::ContextCompactionPolicy;

/// One indexed transcript row used for range selection.
#[derive(Debug, Clone)]
pub struct TranscriptIndexRow {
    pub node_id: String,
    pub event_order: u64,
}

/// Selected prefix eligible for compaction plus retained recent tail boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactableRange {
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub covered_node_ids: Vec<String>,
    pub recent_tail_start_event_order: u64,
}

/// Returns `None` when compaction should not run (too few rows, unresolved work, etc.).
#[must_use]
pub fn select_compactable_range(
    index_rows: &[TranscriptIndexRow],
    items: &[ProvenanceConversationContextItem],
    policy: &ContextCompactionPolicy,
    awaiting_input: bool,
    in_flight_tool_work: bool,
) -> Option<CompactableRange> {
    if index_rows.is_empty() || items.len() < policy.item_threshold {
        return None;
    }
    if awaiting_input || in_flight_tool_work {
        return None;
    }

    let tail_keep = policy.recent_tail_retention.max(1);
    if index_rows.len() <= tail_keep {
        return None;
    }

    let split_at = index_rows.len().saturating_sub(tail_keep);
    let prefix_rows = &index_rows[..split_at];
    let tail_rows = &index_rows[split_at..];

    if prefix_rows.is_empty() {
        return None;
    }

    // Do not compact rows that represent live planning obligations in the tail window.
    let tail_start = tail_rows.first().map(|r| r.event_order).unwrap_or(u64::MAX);

    let covered_start = prefix_rows.first().map(|r| r.event_order).unwrap_or(0);
    let covered_end = prefix_rows
        .last()
        .map(|r| r.event_order)
        .unwrap_or(covered_start);

    let covered_node_ids: Vec<String> = prefix_rows.iter().map(|r| r.node_id.clone()).collect();

    Some(CompactableRange {
        covered_event_order_start: covered_start,
        covered_event_order_end: covered_end,
        covered_node_ids,
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

    fn row(node: &str, order: u64) -> TranscriptIndexRow {
        TranscriptIndexRow {
            node_id: node.to_string(),
            event_order: order,
        }
    }

    #[test]
    fn selects_prefix_leaving_recent_tail() {
        let rows: Vec<_> = (1..=50)
            .map(|i| row(&format!("n{i}"), i as u64 * 10))
            .collect();
        let policy = ContextCompactionPolicy {
            item_threshold: 40,
            recent_tail_retention: 8,
            ..Default::default()
        };
        let range = select_compactable_range(&rows, &[], &policy, false, false).expect("range");
        assert_eq!(range.covered_node_ids.len(), 42);
        assert_eq!(range.recent_tail_start_event_order, 430);
    }

    #[test]
    fn skips_when_awaiting_input() {
        let rows: Vec<_> = (1..=50)
            .map(|i| row(&format!("n{i}"), i as u64 * 10))
            .collect();
        assert!(
            select_compactable_range(&rows, &[], &ContextCompactionPolicy::default(), true, false)
                .is_none()
        );
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
