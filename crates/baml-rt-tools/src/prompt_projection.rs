//! Conversation context projection for BAML prompt injection.
//!
//! Produces a flat `conversation_history` array for `ctx.tags`.
//! Each item has `{role, content}` — rendered by the trait system:
//!
//! - `Message`     → `{HistoryRef} {text}` (e.g. `#1 …`) via [`RefTable::insert_history`]
//! - `ToolCall`    → `{HistoryRef} {describe_invocation_with_hint(...)}`
//! - `ToolResult`  → archive_read render of result value (first 40 lines)
//! - `ToolError`   → archive_read render of error value
//! - `SessionStep` → `Open` / `SendDone` (header ± archive body) / `Read` as the **grep(1)/cat(1)
//!   analogue**: `grep -n 'pat' @N` or `cat -n @N` when no reader; with an [`ArchiveReader`], a
//!   **single** line of **paginated, grep-filtered** archive text (same `grep_paginate` path as
//!   production — never raw JSON dumps). Matches `remotes/semiotic-agentium/pud-squashed`.
//! - `StatusOnly` items are discarded at the conversion boundary before reaching here.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::{
    archive_read::{PageLimit, session_read_command_line},
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
    /// Tool invocation. `args` is the BAML step payload `{"op":"Send","input":{...}}`
    /// forwarded directly to `ToolHandler::describe_invocation`.
    ToolCall {
        tool_name: String,
        args: Value,
    },
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
    One(String),
    /// Two attributed entries, first then second (e.g. send_done header + inline content).
    Two(String, String),
}

/// Callback that re-derives cat-n output from an archive entry.
/// Arguments: `(archive_ref, grep_pattern, offset, limit)`.
pub type ArchiveReader<'a> = &'a dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>;

/// Line caps and tool-call behaviour for [`render_projection_content`].
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
            tool_result: PageLimit::new(40),
            tool_error: PageLimit::new(10),
            send_done: PageLimit::default(),
            tool_call_fallback_json: false,
        }
    }
}

/// Wider caps for episode replay / UI session-history mirroring (still bounded by [`PageLimit::MAX`]).
#[must_use]
pub fn episode_session_history_projection_options() -> ProjectionRenderOptions {
    ProjectionRenderOptions {
        tool_result: PageLimit::new(PageLimit::MAX),
        tool_error: PageLimit::new(PageLimit::MAX),
        send_done: PageLimit::default(),
        tool_call_fallback_json: true,
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
    let mut history = Vec::with_capacity(items.len());
    let mut inlined_archive_refs: HashSet<String> = HashSet::new();
    let mut inlined_read_pages: HashSet<String> = HashSet::new();

    for item in items {
        match render_projection_content_with_state(
            &item,
            registry,
            ref_table,
            archive_reader,
            ProjectionRenderOptions::default(),
            &mut inlined_archive_refs,
            &mut inlined_read_pages,
        ) {
            RenderedEntry::Filtered => {}
            RenderedEntry::One(c) => {
                history.push(json!({ "role": item.role, "content": ensure_entry_boundary(c) }));
            }
            RenderedEntry::Two(first, second) => {
                history.push(json!({ "role": item.role, "content": ensure_entry_boundary(first) }));
                history.push(json!({ "role": item.role, "content": ensure_entry_boundary(second) }));
            }
        }
    }

    Value::Array(history)
}

/// Same rules as [`project_prompt_context`], as `(role, content)` pairs for one item.
#[must_use]
pub fn projection_history_pairs(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
) -> Vec<(String, String)> {
    let role = item.role.clone();
    match render_projection_content(item, registry, ref_table, archive_reader, opts) {
        RenderedEntry::Filtered => Vec::new(),
        RenderedEntry::One(c) => vec![(role, c)],
        // SendDone only: header stays on the step role; archive body is a Read analogue for UI.
        RenderedEntry::Two(a, b) => vec![(role.clone(), a), ("read".to_string(), b)],
    }
}

/// Render one projection item using the same rules as prompt injection, with explicit caps.
#[must_use]
pub fn render_projection_content(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
) -> RenderedEntry {
    let mut inlined_archive_refs = HashSet::new();
    let mut inlined_read_pages = HashSet::new();
    render_projection_content_with_state(
        item,
        registry,
        ref_table,
        archive_reader,
        opts,
        &mut inlined_archive_refs,
        &mut inlined_read_pages,
    )
}

fn read_view_key(archive_ref: &str, grep: Option<&str>, offset: usize, limit: usize) -> String {
    format!("{archive_ref}|{}|{offset}|{limit}", grep.unwrap_or(""))
}

fn ensure_trailing_newline(s: String) -> String {
    if s.ends_with('\n') {
        s
    } else {
        format!("{s}\n")
    }
}

fn ensure_entry_boundary(s: String) -> String {
    let with_newline = ensure_trailing_newline(s);
    if with_newline.ends_with("\n\n") {
        with_newline
    } else {
        format!("{with_newline}\n")
    }
}

fn render_projection_content_with_state(
    item: &PromptProjectionItem,
    registry: &ToolRegistry,
    ref_table: &RefTable,
    archive_reader: Option<ArchiveReader<'_>>,
    opts: ProjectionRenderOptions,
    inlined_archive_refs: &mut HashSet<String>,
    inlined_read_pages: &mut HashSet<String>,
) -> RenderedEntry {
    match &item.content {
        PromptProjectionContent::Message(text) => {
            if text.trim().is_empty() {
                return RenderedEntry::Filtered;
            }
            let h = ref_table.insert_history(
                HistoryEntry::new(item.activity_anchor.clone(), "message".to_string()),
                text.as_str(),
            );
            RenderedEntry::One(format!("{h} {text}"))
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
            RenderedEntry::One(format!("{h} {desc}"))
        }

        PromptProjectionContent::ToolResult { tool_name, result } => {
            let rendered = crate::archive_read::render_to_lines(result);
            let page = crate::archive_read::grep_paginate(
                &rendered,
                None,
                crate::archive_read::LineOffset::default(),
                opts.tool_result,
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
                opts.tool_error,
            );
            let formatted = crate::archive_read::format_cat_n(&page.lines);
            if formatted.trim().is_empty() {
                RenderedEntry::Filtered
            } else {
                RenderedEntry::One(format!("{tool_name} [error]:\n{formatted}"))
            }
        }

        PromptProjectionContent::SessionStep { tool_name, op } => match op {
            SessionStepProjection::Open => {
                let open_text = registry
                    .describe_open_for(tool_name)
                    .unwrap_or_else(|| format!("{tool_name} session opened"));
                // Keep an explicit trailing newline so adjacent same-role session-step rows
                // (e.g. Open followed by SendDone header) never collapse into one token run.
                RenderedEntry::One(format!("{open_text}\n"))
            },
            SessionStepProjection::SendDone {
                archive_ref,
                header,
            } => {
                if inlined_archive_refs.contains(archive_ref) {
                    return RenderedEntry::Two(
                        ensure_trailing_newline(header.clone()),
                        ensure_trailing_newline(format!("cat -n {archive_ref}")),
                    );
                }
                inlined_archive_refs.insert(archive_ref.clone());

                match archive_reader.and_then(|r| r(archive_ref, None, 0, opts.send_done.get())) {
                    Some(content) => {
                        inlined_read_pages.insert(read_view_key(
                            archive_ref,
                            None,
                            0,
                            opts.send_done.get(),
                        ));
                        RenderedEntry::Two(
                            ensure_trailing_newline(header.clone()),
                            ensure_trailing_newline(content),
                        )
                    }
                    None => RenderedEntry::One(ensure_trailing_newline(header.clone())),
                }
            }
            SessionStepProjection::Read {
                archive_ref,
                grep,
                offset,
                limit,
            } => {
                // pud-squashed: command line when we cannot resolve the archive; otherwise the
                // reader returns grep_paginate/format_cat_n output only (controlled, not raw).
                let cmd = session_read_command_line(archive_ref, grep.as_deref());
                let read_key = read_view_key(archive_ref, grep.as_deref(), *offset, *limit);
                if inlined_read_pages.contains(&read_key) {
                    return RenderedEntry::One(ensure_trailing_newline(cmd));
                }
                match archive_reader.and_then(|r| r(archive_ref, grep.as_deref(), *offset, *limit))
                {
                    Some(output) => {
                        inlined_read_pages.insert(read_key);
                        RenderedEntry::One(ensure_trailing_newline(output))
                    }
                    None => RenderedEntry::One(ensure_trailing_newline(cmd)),
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{archive_refs::RefTable, tools::ToolRegistry};

    #[test]
    fn message_history_line_includes_history_ref_prefix() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 0,
            activity_anchor: "evt-1".to_string(),
            role: "user".to_string(),
            content: PromptProjectionContent::Message("what can you do".to_string()),
        }];
        let history = project_prompt_context(items, &registry, &ref_table, None);
        let arr = history.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["content"].as_str(),
            Some("#1 what can you do\n\n"),
            "first history line allocates #1 for citation/drift alignment with explicit entry boundary"
        );
    }

    #[test]
    fn repeated_send_done_for_same_archive_ref_should_not_reinline_payload() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![
            PromptProjectionItem {
                timestamp_ms: 1,
                activity_anchor: "evt-1".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                },
            },
            PromptProjectionItem {
                timestamp_ms: 2,
                activity_anchor: "evt-2".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                },
            },
            PromptProjectionItem {
                timestamp_ms: 3,
                activity_anchor: "evt-3".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                },
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
            payload_occurrences, 1,
            "archive payload should be inlined once per archive_ref to prevent history snowballing"
        );
    }

    #[test]
    fn repeated_read_same_view_should_not_reinline_payload() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![
            PromptProjectionItem {
                timestamp_ms: 1,
                activity_anchor: "evt-r1".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::Read {
                        archive_ref: "@15".to_string(),
                        grep: None,
                        offset: 0,
                        limit: 200,
                    },
                },
            },
            PromptProjectionItem {
                timestamp_ms: 2,
                activity_anchor: "evt-r2".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::Read {
                        archive_ref: "@15".to_string(),
                        grep: None,
                        offset: 0,
                        limit: 200,
                    },
                },
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
            payload_occurrences, 1,
            "read payload should be inlined once per identical read view"
        );
    }

    #[test]
    fn read_default_view_after_send_done_should_not_reinline_payload() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![
            PromptProjectionItem {
                timestamp_ms: 1,
                activity_anchor: "evt-s1".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::SendDone {
                        archive_ref: "@15".to_string(),
                        header: "@15 clickup/get_tasks 'found tasks' [209 lines, 6.3KB]"
                            .to_string(),
                    },
                },
            },
            PromptProjectionItem {
                timestamp_ms: 2,
                activity_anchor: "evt-r1".to_string(),
                role: "assistant".to_string(),
                content: PromptProjectionContent::SessionStep {
                    tool_name: "clickup/get_tasks".to_string(),
                    op: SessionStepProjection::Read {
                        archive_ref: "@15".to_string(),
                        grep: None,
                        offset: 0,
                        limit: 200,
                    },
                },
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
            payload_occurrences, 1,
            "default Read view should be compacted when SendDone already inlined the same page"
        );
    }
}
