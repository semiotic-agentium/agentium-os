// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Convert conversation view rows into [`baml_rt_tools::prompt_projection`] items (feeds
//! `conversation_transcript` via `format_conversation_history_transcript` in the QuickJS host).

use baml_rt_tools::{
    archive_refs::RefTable,
    prompt_projection::{
        ArchiveReader, ProjectionRenderOptions, PromptProjectionContent, PromptProjectionItem,
        SessionStepPayload, SessionStepProjection, projection_history_pairs,
    },
    tools::ToolRegistry,
};

use crate::view::{
    ConversationItemContent, ProvenanceConversationContextItem, SessionStepOp, ToolOutcome,
};

/// Convert a provenance conversation item to a projection item.
///
/// Returns `None` for `StatusOnly` tool results — they carry no meaningful content
/// and are discarded here rather than being filtered later at render time.
pub fn provenance_item_to_projection_item(
    item: ProvenanceConversationContextItem,
) -> Option<PromptProjectionItem> {
    let content = match item.content {
        ConversationItemContent::Message { text, citations } => PromptProjectionContent::Message {
            text,
            citations: citations
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        },
        ConversationItemContent::ToolCall(tc) => PromptProjectionContent::ToolCall {
            tool_name: tc.tool_name,
            args: tc.args,
        },
        ConversationItemContent::ToolResult(tr) => match tr.outcome {
            ToolOutcome::Result(value) => PromptProjectionContent::ToolResult {
                tool_name: tr.tool_name,
                result: value,
            },
            ToolOutcome::Error(value) => PromptProjectionContent::ToolError {
                tool_name: tr.tool_name,
                error: value,
            },
            ToolOutcome::StatusOnly => return None,
        },
        ConversationItemContent::Operational(_) => return None,
        ConversationItemContent::SessionStep(step) => {
            let projection_op = match step.op {
                SessionStepOp::Open => SessionStepProjection::Open,
                SessionStepOp::SendDone {
                    archive_ref,
                    header,
                    informed_by: _,
                } => SessionStepProjection::SendDone {
                    archive_ref,
                    header,
                },
                SessionStepOp::SearchRead {
                    archive_ref,
                    grep,
                    offset,
                    limit,
                } => SessionStepProjection::SearchRead {
                    archive_ref,
                    grep,
                    offset,
                    limit,
                },
                SessionStepOp::PageRead {
                    archive_ref,
                    offset,
                    limit,
                } => SessionStepProjection::PageRead {
                    archive_ref,
                    offset,
                    limit,
                },
            };
            PromptProjectionContent::SessionStep(SessionStepPayload {
                tool_name: step.tool_name,
                op: projection_op,
                send_done_replay_payload: step.send_done_replay_payload,
                read_replay_lines: step.read_replay_lines,
            })
        }
    };
    Some(PromptProjectionItem {
        timestamp_ms: item.timestamp_ms,
        activity_anchor: item.activity_anchor.as_str().to_string(),
        role: item.role,
        content,
    })
}

/// Line pairs `(role, content)` before wire citation prefixing, matching
/// [`baml_rt_tools::prompt_projection::project_projection_item_to_rows`]. Message `citations` are
/// not returned; use
/// [`project_projection_item_to_rows`] for full metadata.
#[must_use]
pub fn projection_pairs_for_conv_item(
    item: &ProvenanceConversationContextItem,
    tool_registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
) -> Option<Vec<(String, String)>> {
    if !item.content.is_meaningful() {
        return None;
    }

    let proj = provenance_item_to_projection_item(item.clone())?;
    Some(projection_history_pairs(
        &proj,
        tool_registry,
        ref_table,
        archive_reader,
        opts,
    ))
}

#[cfg(test)]
mod tests {
    use baml_rt_core::{Citation, ids::ActivityAnchorId};

    use super::*;

    #[test]
    fn message_maps_citations_into_projection() {
        let c = Citation::try_new("#1").expect("citation");
        let item = ProvenanceConversationContextItem {
            timestamp_ms: 1,
            activity_anchor: ActivityAnchorId::from_counter(1),
            role: "assistant".into(),
            content: ConversationItemContent::Message {
                text: "hi".into(),
                citations: vec![c],
            },
            user_speaker_kind: None,
        };
        let p = provenance_item_to_projection_item(item).expect("proj");
        match p.content {
            PromptProjectionContent::Message { text, citations } => {
                assert_eq!(text, "hi");
                assert_eq!(citations, vec!["#1"]);
            }
            _ => panic!("expected Message"),
        }
    }
}
