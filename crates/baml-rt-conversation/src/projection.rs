//! Convert conversation view rows into [`baml_rt_tools::prompt_projection`] items for BAML
//! `ctx.tags['conversation_history']`.

use baml_rt_tools::{
    archive_read::{PageLimit, format_session_read_body_from_json_value},
    archive_refs::RefTable,
    prompt_projection::{
        ArchiveReader, ProjectionRenderOptions, PromptProjectionContent, PromptProjectionItem,
        SessionStepProjection, projection_history_pairs,
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
        ConversationItemContent::Message { text, .. } => PromptProjectionContent::Message(text),
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
            PromptProjectionContent::SessionStep {
                tool_name: step.tool_name,
                op: projection_op,
            }
        }
    };
    Some(PromptProjectionItem {
        timestamp_ms: item.timestamp_ms,
        activity_anchor: item.activity_anchor.as_str().to_string(),
        role: item.role,
        content,
    })
}

/// `SendDone` session line: JSON replay → cat-n body (same cap as prompt projection `send_done`).
pub fn session_history_body_from_send_done_replay(
    payload: &serde_json::Value,
    archive_ref: &str,
    limit: usize,
) -> Option<String> {
    format_session_read_body_from_json_value(payload, archive_ref, None, 0, PageLimit::new(limit))
}

/// Line pairs `(role, content)` before wire citation prefixing, matching
/// [`projection_history_pairs`] / SendDone replay split used for BAML `conversation_history`.
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

    if let ConversationItemContent::SessionStep(ss) = &item.content
        && let SessionStepOp::SendDone {
            archive_ref,
            header,
            ..
        } = &ss.op
        && let Some(payload) = ss.send_done_replay_payload.as_ref()
        && let Some(body) = session_history_body_from_send_done_replay(
            payload,
            archive_ref.as_str(),
            opts.send_done.get(),
        )
    {
        return Some(vec![
            (item.role.clone(), header.clone()),
            ("read".to_string(), body),
        ]);
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
