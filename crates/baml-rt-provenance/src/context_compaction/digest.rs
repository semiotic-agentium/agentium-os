// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Format planning obligations and recent-tail previews for compaction BAML inputs.

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_tools::{archive_refs::RefTable, tools::ToolRegistry};

use super::render::render_items_with_ref_table;

/// Live planning rows from the sealed prefix, one line each.
#[must_use]
pub fn format_planning_digest(
    prefix_items: &[ProvenanceConversationContextItem],
) -> Option<String> {
    let lines: Vec<String> = prefix_items
        .iter()
        .filter(|item| matches!(item.content, ConversationItemContent::Planning(_)))
        .filter_map(|item| {
            let ConversationItemContent::Planning(plan) = &item.content else {
                return None;
            };
            if !plan.is_meaningful() {
                return None;
            }
            let status = plan
                .new_status
                .as_deref()
                .map(|s| format!(" status={s}"))
                .unwrap_or_default();
            Some(format!("- {:?}: {}{}", plan.kind, plan.summary, status))
        })
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Rendered transcript of tail rows retained verbatim after compaction.
#[must_use]
pub fn format_tail_preview(
    tail_items: &[ProvenanceConversationContextItem],
    tool_registry: &ToolRegistry,
    ref_table: &RefTable,
) -> Option<String> {
    if tail_items.is_empty() {
        return None;
    }
    let rendered = render_items_with_ref_table(tail_items, tool_registry, ref_table);
    if rendered.trim().is_empty() {
        None
    } else {
        Some(rendered)
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_conversation::planning::{PlanningEventContent, PlanningEventKind};
    use baml_rt_core::ids::ActivityAnchorId;

    use super::*;

    fn planning_item(
        kind: PlanningEventKind,
        summary: &str,
        status: Option<&str>,
    ) -> ProvenanceConversationContextItem {
        ProvenanceConversationContextItem {
            timestamp_ms: 1,
            activity_anchor: ActivityAnchorId::from("plan-a"),
            role: "system".into(),
            content: ConversationItemContent::Planning(PlanningEventContent {
                kind,
                summary: summary.into(),
                detail: None,
                intent_id: None,
                plan_id: Some("p1".into()),
                step_id: Some("s1".into()),
                old_status: None,
                new_status: status.map(str::to_owned),
            }),
            user_speaker_kind: None,
        }
    }

    #[test]
    fn format_planning_digest_includes_live_obligations() {
        let items = vec![
            planning_item(PlanningEventKind::IntentResolved, "build feature", None),
            planning_item(
                PlanningEventKind::PlanStepStatusChanged,
                "deploy step",
                Some("in_progress"),
            ),
            ProvenanceConversationContextItem {
                timestamp_ms: 2,
                activity_anchor: ActivityAnchorId::from("msg-a"),
                role: "user".into(),
                content: ConversationItemContent::Message {
                    text: "hello".into(),
                    citations: vec![],
                },
                user_speaker_kind: None,
            },
        ];
        let digest = format_planning_digest(&items).expect("digest");
        assert!(digest.contains("build feature"));
        assert!(digest.contains("deploy step"));
        assert!(digest.contains("in_progress"));
    }

    #[test]
    fn format_planning_digest_none_when_no_live_obligations() {
        let items = vec![ProvenanceConversationContextItem {
            timestamp_ms: 1,
            activity_anchor: ActivityAnchorId::from("msg-a"),
            role: "user".into(),
            content: ConversationItemContent::Message {
                text: "hello".into(),
                citations: vec![],
            },
            user_speaker_kind: None,
        }];
        assert!(format_planning_digest(&items).is_none());
    }

    #[test]
    fn format_tail_preview_renders_messages() {
        let items = vec![ProvenanceConversationContextItem {
            timestamp_ms: 1,
            activity_anchor: ActivityAnchorId::from("msg-a"),
            role: "user".into(),
            content: ConversationItemContent::Message {
                text: "recent ping".into(),
                citations: vec![],
            },
            user_speaker_kind: None,
        }];
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let preview = format_tail_preview(&items, &registry, &ref_table).expect("preview");
        assert!(preview.contains("recent ping"));
    }
}
