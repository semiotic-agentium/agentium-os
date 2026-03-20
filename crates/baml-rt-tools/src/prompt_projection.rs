//! Conversation context projection for BAML prompt injection.
//!
//! Produces a flat `conversation_history` array for `ctx.tags`.
//! Each item has `{role, content}` — rendered by the trait system:
//!
//! - `Message`     → text as-is
//! - `ToolCall`    → `describe_invocation_with_hint` (DescribeAction trait)
//! - `ToolResult`  → archive_read render of result value (first 40 lines)
//! - `ToolError`   → archive_read render of error value
//! - `StatusOnly` items are discarded at the conversion boundary before reaching here.

use serde_json::{Value, json};

use crate::tools::ToolRegistry;

/// Session step op for prompt projection — mirrors `SessionStepOp` from provenance.
#[derive(Debug, Clone)]
pub enum SessionStepProjection {
    Open,
    /// `header` is the full display: `"@1 tool_name 'summary' [N lines, KB]"`.
    SendDone {
        archive_ref: String,
        header: String,
    },
    Read {
        archive_ref: String,
        grep: Option<String>,
        offset: usize,
        limit: usize,
    },
}

/// Typed content for a conversation projection item.
/// The `source` string discriminant is replaced by the variant itself.
/// `StatusOnly` results are never present here — they are discarded at the
/// `baml-rt-a2a` conversion boundary before `PromptProjectionItem` is constructed.
#[derive(Debug, Clone)]
pub enum PromptProjectionContent {
    Message(String),
    /// Tool invocation — description is resolved at projection build time (in `to_projection_item`)
    /// so no Value reaches this level.
    ToolCall(String),
    /// Tool result with actual data.
    ToolResult {
        tool_name: String,
        result: Value,
    },
    /// Tool returned an error.
    ToolError {
        tool_name: String,
        error: Value,
    },
    /// An individual step within an in-progress session.
    SessionStep {
        tool_name: String,
        op: SessionStepProjection,
    },
}

#[derive(Debug, Clone)]
pub struct PromptProjectionItem {
    pub timestamp_ms: u64,
    pub event_id: String,
    pub role: String,
    pub content: PromptProjectionContent,
}

/// Output from rendering a single projection item.
///
/// A single item in the source can map to zero, one, or two attributed
/// history entries (e.g. `SendDone` emits the archive header then the
/// inline content as separate entries so both carry the `assistant:` prefix).
pub enum RenderedEntry {
    /// Item is filtered — contributes nothing to conversation history.
    Filtered,
    /// Single attributed entry.
    One(String),
    /// Two attributed entries, first then second (e.g. send_done header + inline content).
    Two(String, String),
}

/// Callback that re-derives cat-n output from an archive entry.
/// Arguments: `(archive_ref, grep_pattern, offset, limit)`.
pub type ArchiveReader<'a> = &'a dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>;

/// Produce the `conversation_history` array for `ctx.tags`.
///
/// `archive_reader`: called for `SessionStep` items to re-derive cat-n output
/// from the archive.
pub fn project_prompt_context(
    items: Vec<PromptProjectionItem>,
    registry: &ToolRegistry,
    archive_reader: Option<ArchiveReader<'_>>,
) -> Value {
    let mut history = Vec::with_capacity(items.len());
    for item in items {
        match render_content(&item, registry, archive_reader) {
            RenderedEntry::Filtered => {}
            RenderedEntry::One(c) => {
                history.push(json!({ "role": item.role, "content": c }));
            }
            RenderedEntry::Two(first, second) => {
                history.push(json!({ "role": item.role, "content": first }));
                history.push(json!({ "role": item.role, "content": second }));
            }
        }
    }
    Value::Array(history)
}

fn render_content(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    archive_reader: Option<ArchiveReader<'_>>,
) -> RenderedEntry {
    match &item.content {
        PromptProjectionContent::Message(text) => {
            if text.trim().is_empty() {
                RenderedEntry::Filtered
            } else {
                RenderedEntry::One(text.clone())
            }
        }

        PromptProjectionContent::ToolCall(desc) => {
            if desc.is_empty() {
                RenderedEntry::Filtered
            } else {
                RenderedEntry::One(desc.clone())
            }
        }

        PromptProjectionContent::ToolResult { tool_name, result } => {
            let rendered = crate::archive_read::render_to_lines(result);
            let page = crate::archive_read::grep_paginate(
                &rendered,
                None,
                crate::archive_read::LineOffset::default(),
                crate::archive_read::PageLimit::new(40),
            );
            let formatted = crate::archive_read::format_cat_n(&page.lines);
            if formatted.trim().is_empty() {
                RenderedEntry::Filtered
            } else {
                RenderedEntry::One(format!("{tool_name}:\n{formatted}"))
            }
        }

        PromptProjectionContent::ToolError { tool_name, error } => {
            let rendered = crate::archive_read::render_to_lines(error);
            let page = crate::archive_read::grep_paginate(
                &rendered,
                None,
                crate::archive_read::LineOffset::default(),
                crate::archive_read::PageLimit::new(10),
            );
            let formatted = crate::archive_read::format_cat_n(&page.lines);
            if formatted.trim().is_empty() {
                RenderedEntry::Filtered
            } else {
                RenderedEntry::One(format!("{tool_name} [error]:\n{formatted}"))
            }
        }

        PromptProjectionContent::SessionStep { tool_name, op } => {
            match op {
                SessionStepProjection::Open => RenderedEntry::One(
                    registry
                        .describe_open_for(tool_name)
                        .unwrap_or_else(|| format!("{tool_name} session opened")),
                ),
                SessionStepProjection::SendDone {
                    archive_ref,
                    header,
                } => {
                    // Two entries: header attributed to assistant, then the inline content
                    // (CLI command + numbered output) also attributed to assistant.
                    match archive_reader.and_then(|r| {
                        r(
                            archive_ref,
                            None,
                            0,
                            crate::archive_read::PageLimit::DEFAULT,
                        )
                    }) {
                        Some(content) => RenderedEntry::Two(header.clone(), content),
                        None => RenderedEntry::One(header.clone()),
                    }
                }
                SessionStepProjection::Read {
                    archive_ref,
                    grep,
                    offset,
                    limit,
                } => {
                    let cmd = match grep.as_deref().filter(|g| !g.is_empty()) {
                        Some(pat) => format!("grep -n '{pat}' {archive_ref}"),
                        None => format!("cat -n {archive_ref}"),
                    };
                    match archive_reader
                        .and_then(|r| r(archive_ref, grep.as_deref(), *offset, *limit))
                    {
                        Some(output) => RenderedEntry::One(output),
                        None => RenderedEntry::One(cmd),
                    }
                }
            }
        }
    }
}
