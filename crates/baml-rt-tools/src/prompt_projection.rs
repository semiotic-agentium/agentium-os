//! Conversation context projection for BAML prompt injection.
//!
//! Produces a flat `conversation_history` array for `ctx.tags`.
//! Each item has `{role, content}` — rendered by the trait system:
//!
//! - `Message`     → `{HistoryRef} {text}` (e.g. `#1 …`) via [`RefTable::insert_history`], plus
//!   optional `citations: string[]` in the tag JSON when the row is message-sourced and refs are non-empty
//! - `ToolCall`    → `{HistoryRef} {describe_invocation_with_hint(...)}`
//! - `ToolResult`  → archive_read render of result value (first `DEFAULT_TOOL_RESULT_INLINE_LINES` lines)
//! - `ToolError`   → archive_read render of error value
//! - `SessionStep` → same rendering as above; items use role **`tool`** in `conversation_history`
//!   (not `assistant`). `Open` / `SendDone` (header ± archive body) / `Read` as the **grep(1)/cat(1)
//!   analogue**: `SearchRead` → `grep -n 'pat' @N`; `PageRead` → `cat -n @N`; with an [`ArchiveReader`], a
//!   **single** line of **paginated** archive text (same `grep_paginate` path as
//!   production — never raw JSON dumps). Matches `remotes/semiotic-agentium/pud-squashed`.
//! - `StatusOnly` items are discarded at the conversion boundary before reaching here.

use std::{borrow::Cow, fmt};

use serde_json::{Value, json};

use crate::{
    archive_read::{
        DEFAULT_TOOL_RESULT_INLINE_LINES, PageLimit, RenderedContent,
        SEND_DONE_HISTORY_INLINE_LINES, format_send_done_replay_from_json,
        format_session_read_body_from_rendered, session_read_command_line,
    },
    archive_refs::{HistoryEntry, RefTable},
    tools::ToolRegistry,
};

/// Session step op for prompt projection — mirrors `SessionStepOp` from provenance.
#[derive(Debug, Clone)]
pub enum SessionStepProjection {
    Open,
    /// `header` is the full display: `"@1 tool_name 'summary' [N lines, KB]"`.
    SendDone {
        archive_ref: String,
        header: String,
    },
    SearchRead {
        archive_ref: String,
        grep: String,
        offset: usize,
        limit: usize,
    },
    PageRead {
        archive_ref: String,
        offset: usize,
        limit: usize,
    },
}

/// Typed content for a conversation projection item.
/// The `source` string discriminant is replaced by the variant itself.
/// `StatusOnly` results are never present here — they are discarded at the
/// `baml-rt-a2a` conversion boundary before `PromptProjectionItem` is constructed.
/// Session FSM step for projection, plus optional graph-replay fields that must match
/// [`baml_rt_conversation::view::SessionStepContent`].
#[derive(Debug, Clone)]
pub struct SessionStepPayload {
    pub tool_name: String,
    pub op: SessionStepProjection,
    /// `SendDone` only: when set, build the read body from this JSON (Graph `WAS_INFORMED_BY` replay)
    /// with the same `send_done` cap as `archive_reader`, instead of (or before falling back to) archive read.
    pub send_done_replay_payload: Option<Value>,
    /// `SearchRead` / `PageRead` only: pre-hydrated window; wins over `archive_reader` when non-empty.
    pub read_replay_lines: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum PromptProjectionContent {
    /// User/assistant text with optional ref-table citation strings (same vocabulary as `Citation` on graph edges).
    Message {
        text: String,
        /// Wire refs (`#N`, `@K`, …); may be empty.
        citations: Vec<String>,
    },
    /// Tool invocation. `args` is the BAML step payload `{"op":"Send","input":{...}}`
    /// forwarded directly to `ToolHandler::describe_invocation`.
    ToolCall { tool_name: String, args: Value },
    /// Tool result with actual data.
    ToolResult { tool_name: String, result: Value },
    /// Tool returned an error.
    ToolError { tool_name: String, error: Value },
    /// An individual step within an in-progress session.
    SessionStep(SessionStepPayload),
}

#[derive(Debug, Clone)]
pub struct PromptProjectionItem {
    pub timestamp_ms: u64,
    /// Same key as graph `a2a_activity_anchor` / core `ActivityAnchorId` for this history line’s activity.
    pub activity_anchor: String,
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
    One {
        content: String,
        /// `Some` only for [`PromptProjectionContent::Message`] with non-empty graph citations.
        message_citations: Option<Vec<String>>,
    },
    /// Two attributed entries, first then second (e.g. send_done header + inline content; second line
    /// uses role `read`, not `tool`).
    Two(String, String),
}

/// Wire `role` string in `conversation_history` / `session_history` JSON. Use
/// [`Self::read_line`] for the second line of a two-line item (e.g. `SendDone` body) so the
/// `read` literal is not duplicated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectedLineRole(Cow<'static, str>); // `Clone` is cheap for the read-line `Borrowed` case

impl ProjectedLineRole {
    /// Second history row for `SendDone` (header + archive body) and other two-line projections.
    #[must_use]
    pub fn read_line() -> Self {
        Self(Cow::Borrowed("read"))
    }

    #[must_use]
    pub fn from_primary(s: String) -> Self {
        Self(Cow::Owned(s))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectedLineRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One flattened `conversation_history` line after rendering (before any episode ref-prefixing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedHistoryRow {
    pub role: ProjectedLineRole,
    pub content: String,
    /// Non-`None` only for message-sourced rows with at least one citation; omitted from JSON when `None`.
    pub message_citations: Option<Vec<String>>,
}

/// Callback that re-derives cat-n output from an archive entry.
/// Arguments: `(archive_ref, grep_pattern, offset, limit)`.
pub type ArchiveReader<'a> = &'a dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>;

/// Line caps and tool-call behaviour for a single item (see [`project_projection_item_to_rows`]).
#[derive(Debug, Clone, Copy)]
pub struct ProjectionRenderOptions {
    pub tool_result: PageLimit,
    pub tool_error: PageLimit,
    pub send_done: PageLimit,
    /// When the registry yields no invocation description, fall back to pretty-printed JSON args.
    pub tool_call_fallback_json: bool,
}

impl Default for ProjectionRenderOptions {
    fn default() -> Self {
        Self {
            tool_result: PageLimit::new(DEFAULT_TOOL_RESULT_INLINE_LINES),
            tool_error: PageLimit::new(10),
            send_done: PageLimit::new(SEND_DONE_HISTORY_INLINE_LINES),
            tool_call_fallback_json: false,
        }
    }
}

/// Projection options for episode session-history and any surface that mirrors user-visible history.
/// Inline windows stay teaser-sized; archive breadth is via session `Read` steps, not giant `SendDone` dumps.
#[must_use]
pub fn episode_session_history_projection_options() -> ProjectionRenderOptions {
    ProjectionRenderOptions {
        tool_result: PageLimit::new(DEFAULT_TOOL_RESULT_INLINE_LINES),
        tool_error: PageLimit::new(DEFAULT_TOOL_RESULT_INLINE_LINES),
        send_done: PageLimit::new(SEND_DONE_HISTORY_INLINE_LINES),
        tool_call_fallback_json: true,
    }
}

#[must_use]
fn projected_history_row_to_json(row: &ProjectedHistoryRow) -> Value {
    match &row.message_citations {
        Some(c) if !c.is_empty() => json!({
            "role": row.role.as_str(),
            "content": row.content,
            "citations": c
        }),
        _ => json!({ "role": row.role.as_str(), "content": row.content }),
    }
}

/// Turn one [`PromptProjectionItem`] into zero or more [`ProjectedHistoryRow`]s. Rendering is
/// **stateless across items** — each row reflects this graph item only; provenance is not
/// re-written at read time (no cross-item de-duplication of archive views).
pub fn project_projection_item_to_rows(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
) -> Vec<ProjectedHistoryRow> {
    let role_main = ProjectedLineRole::from_primary(item.role.clone());
    match render_projection_content(item, registry, ref_table, archive_reader, opts) {
        RenderedEntry::Filtered => Vec::new(),
        RenderedEntry::One {
            content,
            message_citations,
        } => vec![ProjectedHistoryRow {
            role: role_main,
            content,
            message_citations,
        }],
        RenderedEntry::Two(first, second) => vec![
            ProjectedHistoryRow {
                role: role_main.clone(),
                content: first,
                message_citations: None,
            },
            ProjectedHistoryRow {
                role: ProjectedLineRole::read_line(),
                content: second,
                message_citations: None,
            },
        ],
    }
}

/// Produce the `conversation_history` array for `ctx.tags`.
///
/// `ref_table`: receives `#N` allocations for messages and tool-call descriptions.
/// `archive_reader`: called for `SessionStep` items to re-derive cat-n output from the archive.
pub fn project_prompt_context(
    items: Vec<PromptProjectionItem>,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
) -> Value {
    let mut history: Vec<Value> = Vec::with_capacity(items.len());
    let opts = ProjectionRenderOptions::default();
    for item in items {
        for row in project_projection_item_to_rows(&item, registry, ref_table, archive_reader, opts)
        {
            history.push(projected_history_row_to_json(&row));
        }
    }
    Value::Array(history)
}

/// Same rules as [`project_prompt_context`], as `(role, content)` pairs for one item. Message
/// citation metadata is dropped; use
/// [`project_projection_item_to_rows`] when `citations` are required.
#[must_use]
pub fn projection_history_pairs(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
) -> Vec<(String, String)> {
    project_projection_item_to_rows(item, registry, ref_table, archive_reader, opts)
        .into_iter()
        .map(|r| (r.role.to_string(), r.content))
        .collect()
}

/// Paginated `cat -n` block for tool result / error values (shared caps via [`PageLimit`]).
fn tool_value_to_rendered_entry(
    value: &Value,
    tool_name: &str,
    is_error: bool,
    page_limit: PageLimit,
) -> RenderedEntry {
    let rendered = crate::archive_read::render_to_lines(value);
    let page = crate::archive_read::grep_paginate(
        &rendered,
        None,
        crate::archive_read::LineOffset::default(),
        page_limit,
    );
    let formatted = crate::archive_read::format_cat_n(&page.lines);
    if formatted.trim().is_empty() {
        RenderedEntry::Filtered
    } else {
        let range_comment = page.session_range_comment();
        let base = if is_error {
            format!("{tool_name} [error]")
        } else {
            tool_name.to_string()
        };
        let text = if range_comment.is_empty() {
            format!("{base}:\n{formatted}")
        } else {
            format!("{base}:{range_comment}\n{formatted}")
        };
        RenderedEntry::One {
            content: text,
            message_citations: None,
        }
    }
}

fn render_projection_content(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
) -> RenderedEntry {
    match &item.content {
        PromptProjectionContent::Message { text, citations } => {
            if text.trim().is_empty() {
                return RenderedEntry::Filtered;
            }
            let h = ref_table.insert_history(
                HistoryEntry::new(item.activity_anchor.clone(), "message".to_string()),
                text.as_str(),
            );
            let message_citations = if citations.is_empty() {
                None
            } else {
                Some(citations.clone())
            };
            RenderedEntry::One {
                content: format!("{h} {text}"),
                message_citations,
            }
        }

        PromptProjectionContent::ToolCall { tool_name, args } => {
            let mut desc = registry.describe_invocation_with_hint(Some(tool_name.as_str()), args);
            if desc.trim().is_empty() && opts.tool_call_fallback_json {
                desc = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
            }
            if desc.trim().is_empty() {
                return RenderedEntry::Filtered;
            }
            let h = ref_table.insert_history(
                HistoryEntry::new(item.activity_anchor.clone(), "tool_call".to_string()),
                desc.as_str(),
            );
            RenderedEntry::One {
                content: format!("{h} {desc}"),
                message_citations: None,
            }
        }

        PromptProjectionContent::ToolResult { tool_name, result } => {
            tool_value_to_rendered_entry(result, tool_name, false, opts.tool_result)
        }

        PromptProjectionContent::ToolError { tool_name, error } => {
            tool_value_to_rendered_entry(error, tool_name, true, opts.tool_error)
        }

        PromptProjectionContent::SessionStep(s) => {
            let tool_name = s.tool_name.as_str();
            match &s.op {
                SessionStepProjection::Open => RenderedEntry::One {
                    content: registry
                        .describe_open_for(tool_name)
                        .unwrap_or_else(|| format!("{tool_name} session opened")),
                    message_citations: None,
                },
                SessionStepProjection::SendDone {
                    archive_ref,
                    header,
                } => {
                    if let Some(ref payload) = s.send_done_replay_payload
                        && let Some(body) = format_send_done_replay_from_json(
                            payload,
                            archive_ref,
                            PageLimit::new(opts.send_done.get()),
                        )
                    {
                        return RenderedEntry::Two(header.clone(), body);
                    }

                    match archive_reader.and_then(|r| r(archive_ref, None, 0, opts.send_done.get()))
                    {
                        Some(content) => RenderedEntry::Two(header.clone(), content),
                        None => RenderedEntry::One {
                            content: header.clone(),
                            message_citations: None,
                        },
                    }
                }
                SessionStepProjection::SearchRead {
                    archive_ref,
                    grep,
                    offset,
                    limit,
                } => {
                    let cmd = session_read_command_line(archive_ref, Some(grep.as_str()));
                    if let Some(ref lines) = s.read_replay_lines
                        && !lines.is_empty()
                    {
                        let rendered = RenderedContent::from_lines(
                            lines.iter().filter(|l| !l.is_empty()).cloned(),
                        );
                        let out = format_session_read_body_from_rendered(
                            &rendered,
                            archive_ref,
                            Some(grep.as_str()),
                            *offset,
                            PageLimit::new(*limit),
                        );
                        return RenderedEntry::One {
                            content: out,
                            message_citations: None,
                        };
                    }
                    match archive_reader
                        .and_then(|r| r(archive_ref, Some(grep.as_str()), *offset, *limit))
                    {
                        Some(output) => RenderedEntry::One {
                            content: output,
                            message_citations: None,
                        },
                        None => RenderedEntry::One {
                            content: cmd,
                            message_citations: None,
                        },
                    }
                }
                SessionStepProjection::PageRead {
                    archive_ref,
                    offset,
                    limit,
                } => {
                    let cmd = session_read_command_line(archive_ref, None);
                    if let Some(ref lines) = s.read_replay_lines
                        && !lines.is_empty()
                    {
                        let rendered = RenderedContent::from_lines(
                            lines.iter().filter(|l| !l.is_empty()).cloned(),
                        );
                        let out = format_session_read_body_from_rendered(
                            &rendered,
                            archive_ref,
                            None,
                            *offset,
                            PageLimit::new(*limit),
                        );
                        return RenderedEntry::One {
                            content: out,
                            message_citations: None,
                        };
                    }
                    match archive_reader.and_then(|r| r(archive_ref, None, *offset, *limit)) {
                        Some(output) => RenderedEntry::One {
                            content: output,
                            message_citations: None,
                        },
                        None => RenderedEntry::One {
                            content: cmd,
                            message_citations: None,
                        },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{archive_refs::RefTable, tools::ToolRegistry};

    fn session_step(tool_name: &str, op: SessionStepProjection) -> PromptProjectionContent {
        PromptProjectionContent::SessionStep(SessionStepPayload {
            tool_name: tool_name.to_string(),
            op,
            send_done_replay_payload: None,
            read_replay_lines: None,
        })
    }

    #[test]
    fn send_done_two_emits_read_role_on_second_history_row() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 1,
            activity_anchor: "evt-sd".to_string(),
            role: "assistant".to_string(),
            content: session_step(
                "demo/tool",
                SessionStepProjection::SendDone {
                    archive_ref: "@3".to_string(),
                    header: "@3 demo/tool 'ok' [1 lines]".to_string(),
                },
            ),
        }];
        let archive_reader =
            |archive_ref: &str, _grep: Option<&str>, _offset: usize, _limit: usize| {
                Some(format!("cat -n {archive_ref}\nBODY"))
            };
        let history = project_prompt_context(items, &registry, &ref_table, Some(&archive_reader));
        let arr = history.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["role"].as_str(), Some("assistant"));
        assert_eq!(arr[1]["role"].as_str(), Some("read"));
    }

    #[test]
    fn message_history_line_includes_history_ref_prefix() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 0,
            activity_anchor: "evt-1".to_string(),
            role: "user".to_string(),
            content: PromptProjectionContent::Message {
                text: "what can you do".to_string(),
                citations: vec![],
            },
        }];
        let history = project_prompt_context(items, &registry, &ref_table, None);
        let arr = history.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["content"].as_str(),
            Some("#1 what can you do"),
            "first history line allocates #1 for citation/drift alignment"
        );
    }

    #[test]
    fn message_with_citations_includes_them_in_json() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 0,
            activity_anchor: "evt-1".to_string(),
            role: "assistant".to_string(),
            content: PromptProjectionContent::Message {
                text: "see prior".to_string(),
                citations: vec!["#1".to_string(), "@1".to_string()],
            },
        }];
        let history = project_prompt_context(items, &registry, &ref_table, None);
        let arr = history.as_array().expect("array");
        assert_eq!(arr[0]["citations"], json!(["#1", "@1"]));
    }

    #[test]
    fn repeated_send_done_for_same_archive_ref_inlines_each_time() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![
            PromptProjectionItem {
                timestamp_ms: 1,
                activity_anchor: "evt-1".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                ),
            },
            PromptProjectionItem {
                timestamp_ms: 2,
                activity_anchor: "evt-2".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                ),
            },
            PromptProjectionItem {
                timestamp_ms: 3,
                activity_anchor: "evt-3".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                ),
            },
        ];

        let archive_reader =
            |archive_ref: &str, _grep: Option<&str>, _offset: usize, _limit: usize| {
                Some(format!("cat -n {archive_ref}\nTASK_LIST_PAYLOAD"))
            };

        let history = project_prompt_context(items, &registry, &ref_table, Some(&archive_reader));
        let payload_occurrences = history
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|item| item.get("content").and_then(Value::as_str))
            .filter(|content| content.contains("TASK_LIST_PAYLOAD"))
            .count();

        assert_eq!(
            payload_occurrences, 3,
            "each graph SendDone is rendered; no read-time dedup of archive body"
        );
    }

    #[test]
    fn repeated_read_same_view_inlines_each_time() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![
            PromptProjectionItem {
                timestamp_ms: 1,
                activity_anchor: "evt-r1".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::PageRead {
                        archive_ref: "@15".to_string(),
                        offset: 0,
                        limit: 200,
                    },
                ),
            },
            PromptProjectionItem {
                timestamp_ms: 2,
                activity_anchor: "evt-r2".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::PageRead {
                        archive_ref: "@15".to_string(),
                        offset: 0,
                        limit: 200,
                    },
                ),
            },
        ];

        let archive_reader =
            |archive_ref: &str, _grep: Option<&str>, offset: usize, limit: usize| {
                Some(format!(
                    "cat -n {archive_ref}  # lines {}-{}\nREAD_PAGE_PAYLOAD",
                    offset + 1,
                    offset + limit
                ))
            };

        let history = project_prompt_context(items, &registry, &ref_table, Some(&archive_reader));
        let payload_occurrences = history
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|item| item.get("content").and_then(Value::as_str))
            .filter(|content| content.contains("READ_PAGE_PAYLOAD"))
            .count();

        assert_eq!(
            payload_occurrences, 2,
            "each graph PageRead is rendered in full; no read-time view dedup"
        );
    }

    #[test]
    fn read_default_view_after_send_done_inlines_again() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![
            PromptProjectionItem {
                timestamp_ms: 1,
                activity_anchor: "evt-s1".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                ),
            },
            PromptProjectionItem {
                timestamp_ms: 2,
                activity_anchor: "evt-r1".to_string(),
                role: "assistant".to_string(),
                content: session_step(
                    "clickup/get_tasks",
                    SessionStepProjection::PageRead {
                        archive_ref: "@15".to_string(),
                        offset: 0,
                        limit: 200,
                    },
                ),
            },
        ];

        let archive_reader =
            |archive_ref: &str, _grep: Option<&str>, offset: usize, limit: usize| {
                Some(format!(
                    "cat -n {archive_ref}  # lines {}-{}\nCOMBINED_VIEW_PAYLOAD",
                    offset + 1,
                    offset + limit
                ))
            };

        let history = project_prompt_context(items, &registry, &ref_table, Some(&archive_reader));
        let payload_occurrences = history
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|item| item.get("content").and_then(Value::as_str))
            .filter(|content| content.contains("COMBINED_VIEW_PAYLOAD"))
            .count();

        assert_eq!(
            payload_occurrences, 2,
            "SendDone and a later PageRead of the same @N are both fully rendered"
        );
    }

    #[test]
    fn tool_result_includes_pagination_hint_when_truncated() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let rows: Vec<Value> = (0..100).map(|i| json!(format!("line{i}"))).collect();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 1,
            activity_anchor: "evt-tr".into(),
            role: "tool".into(),
            content: PromptProjectionContent::ToolResult {
                tool_name: "demo/tool".into(),
                result: Value::Array(rows),
            },
        }];
        let history = project_prompt_context(items, &registry, &ref_table, None);
        let arr = history.as_array().expect("array");
        let content = arr[0]["content"].as_str().expect("content");
        assert!(
            content.contains("more — offset="),
            "expected pagination footer in: {content}"
        );
        assert!(
            content.contains(&format!("offset={DEFAULT_TOOL_RESULT_INLINE_LINES}")),
            "expected default cap offset in: {content}"
        );
    }

    #[test]
    fn live_default_options_differ_from_episode_session_history_options() {
        use crate::archive_read::DEFAULT_TOOL_RESULT_INLINE_LINES;

        let d = ProjectionRenderOptions::default();
        let e = episode_session_history_projection_options();
        assert!(!d.tool_call_fallback_json);
        assert!(e.tool_call_fallback_json);
        assert_eq!(d.tool_error.get(), 10);
        assert_eq!(e.tool_error.get(), DEFAULT_TOOL_RESULT_INLINE_LINES);
        assert_eq!(d.tool_result.get(), e.tool_result.get());
        assert_eq!(d.send_done.get(), e.send_done.get());
    }
}
