//! Convert provenance conversation rows into [`baml_rt_tools::prompt_projection`] items.
//!
//! Shared with the A2A transport path so citation resolution rebuilds the same
//! `#N` / `@N` [`baml_rt_tools::archive_refs::RefTable`] the LLM saw in `ctx.tags['conversation_history']`.

use baml_rt_tools::prompt_projection::{
    PromptProjectionContent, PromptProjectionItem, SessionStepProjection,
};

use crate::store::{
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
