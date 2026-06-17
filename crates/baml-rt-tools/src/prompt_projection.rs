// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conversation context projection for prompts.
//!
//! Produces a flat JSON array of `{role, content}` rows for graph/API consumers; the QuickJS host
//! injects **only** [`format_conversation_history_transcript`] as `ctx.tags['conversation_transcript']`
//! into BAML (canonical history surface).
//! Each item has `{role, content}` — rendered by the trait system:
//!
//! - `Message`     → `{HistoryRef} {text}` (e.g. `#1 …`) via [`RefTable::insert_history`], plus
//!   optional `citations: string[]` in the tag JSON when the row is message-sourced and refs are non-empty
//! - `ToolCall`    → `{HistoryRef} {describe_invocation_with_hint(...)}`
//! - `ToolResult`  → archive_read render of result value (first `DEFAULT_TOOL_RESULT_INLINE_LINES` lines);
//!   top-level and nested `citations: []` are stripped before render (no vacuous lines in history)
//! - `ToolError`   → archive_read render of error value (same `citations` strip as tool results)
//! - `SessionStep` → same rendering as above; items use role **`tool`** in `conversation_history`
//!   (not `assistant`). `Open` and **`SendDone` are omitted** from projected history
//!   (FSM bookkeeping only — graph rows stay). Archive **content** appears only for explicit
//!   `SearchRead` / `PageRead` via [`ArchiveReader`] and `grep_paginate` — never raw JSON dumps.
//! - `StatusOnly` items are discarded at the conversion boundary before reaching here.

use std::{borrow::Cow, fmt};

use baml_rt_core::is_history_infrastructure_notice;
use serde_json::{Map, Value, json};

use crate::{
    archive_read::{
        DEFAULT_TOOL_RESULT_INLINE_LINES, PageLimit, RenderedContent, ShortRef,
        format_session_read_body_from_rendered, format_session_read_from_vtable, render_to_lines,
        session_read_command_line,
    },
    archive_refs::{ArchiveEntry, HistoryEntry, RefTable},
    llm_request_display::flatten_message_content_value,
    tools::ToolRegistry,
};

/// Compact `SendDone` header line (same string as the archive table header). Used for logging and
/// APIs that surface session FSM state; **not** inserted into `conversation_history` /
/// `conversation_transcript` projection (those omit `SendDone`; explicit reads carry archive text).
#[must_use]
pub fn format_send_done_projection_line(header: &str, _archive_ref: &str) -> String {
    header.to_string()
}

/// Flatten projected history rows (JSON array of `{role, content}` objects) into chat-style blocks
/// (blank line between turns). `role` and `content` are taken from each row; optional fields are
/// ignored. If `content` is an array of `type: "text"` parts (OpenAI-style), it is flattened to
/// a single string for display, matching [`flatten_message_content_value`].
#[must_use]
pub fn format_conversation_history_transcript(rows: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let role = obj
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let content_raw = obj.get("content");
        let content = match content_raw {
            None => String::new(),
            Some(c) if c.is_string() => c.as_str().unwrap_or("").to_string(),
            Some(c) if c.is_array() => flatten_message_content_value(c)
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| c.to_string()),
            Some(c) => c.to_string(),
        };
        parts.push(format!("{role}: {content}"));
    }
    parts.join("\n\n")
}

/// Session step op for prompt projection — mirrors `SessionStepOp` from provenance.
#[derive(Debug, Clone)]
pub enum SessionStepProjection {
    Open,
    /// `header` is the full display: `"@1 · \"summary\" · NL · size"` (see [`ArchiveEntry::display_header`](crate::archive_refs::ArchiveEntry::display_header)).
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
    /// `SendDone` only: graph-hydrated tool `tool_result` JSON for ref-table / `@N` replay seeding;
    /// the `SendDone` step itself is **not** emitted as a projected history row.
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
    /// Host planning lifecycle row (intent/plan/step status).
    Planning(PlanningProjectionPayload),
    /// Compaction summary replacing a covered transcript prefix.
    CompactionSummary {
        summary: String,
        covered_event_order_start: u64,
        covered_event_order_end: u64,
    },
}

/// Planning lifecycle projection payload (mirrors conversation planning rows).
#[derive(Debug, Clone)]
pub struct PlanningProjectionPayload {
    pub kind: PlanningProjectionKind,
    pub summary: String,
    pub detail: Option<String>,
    pub intent_id: Option<String>,
    pub plan_id: Option<String>,
    pub step_id: Option<String>,
    pub old_status: Option<String>,
    pub new_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningProjectionKind {
    IntentResolved,
    IntentRevised,
    PlanCommitted,
    PlanSuperseded,
    PlanStepStatusChanged,
}

impl PlanningProjectionKind {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::IntentResolved => "intent_resolved",
            Self::IntentRevised => "intent_revised",
            Self::PlanCommitted => "plan_committed",
            Self::PlanSuperseded => "plan_superseded",
            Self::PlanStepStatusChanged => "plan_step_status_changed",
        }
    }
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
/// A single item in the source maps to zero or one attributed history entry
/// (archive body lines are only emitted for explicit `SearchRead` / `PageRead` graph rows).
pub enum RenderedEntry {
    /// Item is filtered — contributes nothing to conversation history.
    Filtered,
    /// Single attributed entry.
    One {
        content: String,
        /// `Some` only for [`PromptProjectionContent::Message`] with non-empty graph citations.
        message_citations: Option<Vec<String>>,
    },
}

/// Wire `role` string in `conversation_history` / `session_history` JSON. Use
/// [`Self::read_line`] for explicit `SearchRead` / `PageRead` tool_result rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectedLineRole(Cow<'static, str>); // `Clone` is cheap for the read-line `Borrowed` case

impl ProjectedLineRole {
    /// History row for a session read step (archive content: `cat -n` / `grep -n` block).
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
    /// When the registry yields no invocation description, fall back to pretty-printed JSON args.
    pub tool_call_fallback_json: bool,
}

impl Default for ProjectionRenderOptions {
    fn default() -> Self {
        Self {
            tool_result: PageLimit::new(DEFAULT_TOOL_RESULT_INLINE_LINES),
            tool_error: PageLimit::new(10),
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
        tool_call_fallback_json: true,
    }
}

/// Recursively remove `citations` when it is an empty array so `conversation_history` does not
/// show vacuous `citations: []` in tool-call args or tool results (e.g. planning emit payloads).
fn strip_vacuous_citation_fields(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                if k == "citations" && v.as_array().is_some_and(|a| a.is_empty()) {
                    continue;
                }
                out.insert(k.clone(), strip_vacuous_citation_fields(v));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(strip_vacuous_citation_fields).collect()),
        _ => value.clone(),
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
    }
}

/// Produce the projected history JSON array (feeds transcript formatting and HTTP snapshots).
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
fn inline_tool_result_range_comment(page: &crate::archive_read::GrepPage) -> String {
    if page.lines.is_empty() {
        return String::new();
    }
    let first = page
        .lines
        .first()
        .map(|l| l.original_line_number)
        .unwrap_or(1);
    let last = page
        .lines
        .last()
        .map(|l| l.original_line_number)
        .unwrap_or(1);
    if page.has_more {
        let remaining = page.total_matched.saturating_sub(page.next_offset);
        format!(
            "  # lines {first}-{last} of {} ({remaining} more not shown in this preview)",
            page.total_matched
        )
    } else if first == 1 && last == page.total_matched {
        String::new()
    } else {
        format!("  # lines {first}-{last} of {}", page.total_matched)
    }
}

fn tool_value_to_rendered_entry(
    value: &Value,
    tool_name: &str,
    is_error: bool,
    page_limit: PageLimit,
) -> RenderedEntry {
    if matches!(
        value,
        Value::Object(map)
            if map.len() == 1
                && matches!(
                    map.get("status").and_then(Value::as_str),
                    Some("sent" | "finished" | "aborted" | "opened")
                )
    ) {
        return RenderedEntry::Filtered;
    }
    let sanitized = strip_vacuous_citation_fields(value);
    let rendered = crate::archive_read::render_to_lines(&sanitized);
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
        let range_comment = inline_tool_result_range_comment(&page);
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

fn seed_send_done_replay_archive(
    ref_table: &RefTable,
    item: &PromptProjectionItem,
    payload: &SessionStepPayload,
    archive_ref: &str,
) {
    let Some(replay_payload) = payload.send_done_replay_payload.as_ref() else {
        return;
    };
    let Some(short_ref) = ShortRef::parse(archive_ref) else {
        return;
    };
    let rendered = render_to_lines(replay_payload);
    if rendered.is_empty() {
        return;
    }
    let entry = ArchiveEntry::new(
        rendered,
        payload.tool_name.clone(),
        Some(format!("{} replayed result", payload.tool_name)),
        item.activity_anchor.clone(),
        "tool_result".to_string(),
    );
    ref_table.insert_virtual_archive_ref(short_ref, entry);
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
            if text.trim().is_empty() || is_history_infrastructure_notice(text) {
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
            let args = strip_vacuous_citation_fields(args);
            let mut desc = registry.describe_invocation_with_hint(Some(tool_name.as_str()), &args);
            if desc.trim().is_empty() && opts.tool_call_fallback_json {
                desc = serde_json::to_string_pretty(&args).unwrap_or_else(|_| args.to_string());
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

        PromptProjectionContent::Planning(plan) => {
            if plan.summary.trim().is_empty() {
                return RenderedEntry::Filtered;
            }
            let mut body = format!("[planning:{}] {}", plan.kind.as_wire_str(), plan.summary);
            if let Some(ref detail) = plan.detail
                && !detail.trim().is_empty()
            {
                body.push_str(&format!(" — {detail}"));
            }
            if let Some(ref step_id) = plan.step_id {
                body.push_str(&format!(" step={step_id}"));
            }
            if let Some(ref new_status) = plan.new_status {
                body.push_str(&format!(" status={new_status}"));
            }
            let h = ref_table.insert_history(
                HistoryEntry::new(item.activity_anchor.clone(), "plan".to_string()),
                body.as_str(),
            );
            RenderedEntry::One {
                content: format!("{h} {body}"),
                message_citations: None,
            }
        }

        PromptProjectionContent::CompactionSummary {
            summary,
            covered_event_order_start,
            covered_event_order_end,
        } => {
            if summary.trim().is_empty() {
                return RenderedEntry::Filtered;
            }
            let body = format!(
                "[compaction summary {covered_event_order_start}..{covered_event_order_end}] {summary}"
            );
            let h = ref_table.insert_history(
                HistoryEntry::new(item.activity_anchor.clone(), "compaction".to_string()),
                body.as_str(),
            );
            RenderedEntry::One {
                content: format!("{h} {body}"),
                message_citations: None,
            }
        }

        PromptProjectionContent::SessionStep(s) => match &s.op {
            SessionStepProjection::Open => RenderedEntry::Filtered,
            SessionStepProjection::SendDone {
                archive_ref,
                header: _,
            } => {
                seed_send_done_replay_archive(ref_table, item, s, archive_ref);
                RenderedEntry::Filtered
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
                if let Some(output) = format_session_read_from_vtable(
                    ref_table,
                    archive_ref,
                    Some(grep.as_str()),
                    *offset,
                    *limit,
                ) {
                    return RenderedEntry::One {
                        content: output,
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
                if let Some(output) =
                    format_session_read_from_vtable(ref_table, archive_ref, None, *offset, *limit)
                {
                    return RenderedEntry::One {
                        content: output,
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
        },
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

    fn session_step_with_replay(
        tool_name: &str,
        op: SessionStepProjection,
        replay_payload: Value,
    ) -> PromptProjectionContent {
        PromptProjectionContent::SessionStep(SessionStepPayload {
            tool_name: tool_name.to_string(),
            op,
            send_done_replay_payload: Some(replay_payload),
            read_replay_lines: None,
        })
    }

    #[test]
    fn send_done_is_filtered_from_projected_history_even_with_archive_reader() {
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
        assert!(
            arr.is_empty(),
            "SendDone must not appear in conversation_history JSON: {history}"
        );
    }

    #[test]
    fn send_done_replay_seeds_read_matrix() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let replay_payload = json!([
            {"agent": "cleese", "answer": "argument accepted"},
            {"agent": "chapman", "answer": "counterpoint found"}
        ]);
        let send_done = session_step_with_replay(
            "system/internal_a2a",
            SessionStepProjection::SendDone {
                archive_ref: "@8".to_string(),
                header: "@8 · \"delegated result\" · 2L · 64B".to_string(),
            },
            replay_payload,
        );

        struct ReadCase {
            label: &'static str,
            read: SessionStepProjection,
            want_in_content: &'static [&'static str],
            want_absent: &'static [&'static str],
        }
        let cases = [
            ReadCase {
                label: "page_read",
                read: SessionStepProjection::PageRead {
                    archive_ref: "@8".to_string(),
                    offset: 0,
                    limit: 20,
                },
                want_in_content: &["cat -n @8", "argument accepted", "counterpoint found"],
                want_absent: &[],
            },
            ReadCase {
                label: "search_read",
                read: SessionStepProjection::SearchRead {
                    archive_ref: "@8".to_string(),
                    grep: "counterpoint".to_string(),
                    offset: 0,
                    limit: 20,
                },
                want_in_content: &["grep -n", "counterpoint found"],
                want_absent: &["argument accepted"],
            },
        ];

        for case in cases {
            let items = vec![
                PromptProjectionItem {
                    timestamp_ms: 1,
                    activity_anchor: format!("evt-send-done-{}", case.label),
                    role: "tool".to_string(),
                    content: send_done.clone(),
                },
                PromptProjectionItem {
                    timestamp_ms: 2,
                    activity_anchor: format!("evt-read-{}", case.label),
                    role: "tool".to_string(),
                    content: session_step("system/internal_a2a", case.read),
                },
            ];
            let history = project_prompt_context(items, &registry, &ref_table, None);
            let rows = history.as_array().expect("array");
            assert_eq!(rows.len(), 1, "{}: SendDone stays filtered", case.label);
            let content = rows[0]["content"].as_str().expect("content");
            for needle in case.want_in_content {
                assert!(content.contains(needle), "{}: missing {needle}", case.label);
            }
            for needle in case.want_absent {
                assert!(
                    !content.contains(needle),
                    "{}: unexpected {needle}",
                    case.label
                );
            }
        }
    }

    #[test]
    fn format_conversation_history_transcript_matrix() {
        let cases: &[(&str, serde_json::Value, &str)] = &[
            (
                "plain",
                json!({"role": "user", "content": "#1 hi"}),
                "user: #1 hi",
            ),
            (
                "openai_parts",
                json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "line1"}]
                }),
                "user: line1",
            ),
        ];
        for (label, row, want) in cases {
            let got = format_conversation_history_transcript(std::slice::from_ref(row));
            assert_eq!(&got, want, "{label}");
        }
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

    /// Spec: one graph row → one `conversation_history` line. (Duplicate user text in the
    /// host-facing API means duplicate `ProvenanceConversationContextItem` messages or a
    /// transport bug — the projector will not merge them.)
    #[test]
    fn three_message_activities_with_same_text_yield_three_distinct_ref_lines() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let same = "hi mate";
        let items: Vec<PromptProjectionItem> = (1..=3)
            .map(|i| PromptProjectionItem {
                timestamp_ms: i,
                activity_anchor: format!("evt-dup-{i}"),
                role: "user".to_string(),
                content: PromptProjectionContent::Message {
                    text: same.to_string(),
                    citations: vec![],
                },
            })
            .collect();
        let history = project_prompt_context(items, &registry, &ref_table, None);
        let arr = history.as_array().expect("array");
        assert_eq!(arr.len(), 3, "one row per graph message activity");
        assert_eq!(arr[0]["content"].as_str(), Some("#1 hi mate"));
        assert_eq!(arr[1]["content"].as_str(), Some("#2 hi mate"));
        assert_eq!(arr[2]["content"].as_str(), Some("#3 hi mate"));
    }

    /// Stable refs: re-running projection on the same items must not churn `#N` (citation-drift
    /// path must match `conversation_history` byte-for-byte for unchanged graph rows).
    #[test]
    fn repeat_projection_byte_identical_when_graph_unchanged() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 0,
            activity_anchor: "evt-stable".to_string(),
            role: "user".to_string(),
            content: PromptProjectionContent::Message {
                text: "hello again".to_string(),
                citations: vec![],
            },
        }];
        let h1 = project_prompt_context(items.clone(), &registry, &ref_table, None);
        let h2 = project_prompt_context(items, &registry, &ref_table, None);
        assert_eq!(h1, h2, "reprojection should reuse the same #N and content");
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
    fn tool_result_strips_empty_citations_from_yaml_block() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let items = vec![PromptProjectionItem {
            timestamp_ms: 0,
            activity_anchor: "evt-tr".to_string(),
            role: "tool".to_string(),
            content: PromptProjectionContent::ToolResult {
                tool_name: "a2a/execution_session_step".to_string(),
                result: json!({ "citations": [], "status": "ok" }),
            },
        }];
        let history = project_prompt_context(items, &registry, &ref_table, None);
        let arr = history.as_array().expect("array");
        let content = arr[0]["content"].as_str().expect("content");
        assert!(
            !content.to_lowercase().contains("citations"),
            "vacuous citations key should not appear: {content}"
        );
    }

    #[test]
    fn projection_yields_empty_history_matrix() {
        let registry = ToolRegistry::new();
        let ref_table = RefTable::new();
        let cases: Vec<Vec<PromptProjectionItem>> = vec![
            vec![PromptProjectionItem {
                timestamp_ms: 0,
                activity_anchor: "evt-open".to_string(),
                role: "tool".to_string(),
                content: session_step("system/discover_agents", SessionStepProjection::Open),
            }],
            vec![
                PromptProjectionItem {
                    timestamp_ms: 0,
                    activity_anchor: "evt-msg-1".to_string(),
                    role: "assistant".to_string(),
                    content: PromptProjectionContent::Message {
                        text: "#2 Calling model: openai/gpt-4o-mini".to_string(),
                        citations: Vec::new(),
                    },
                },
                PromptProjectionItem {
                    timestamp_ms: 1,
                    activity_anchor: "evt-msg-2".to_string(),
                    role: "assistant".to_string(),
                    content: PromptProjectionContent::Message {
                        text: "#3 Invoking tool: system/discover_agents".to_string(),
                        citations: Vec::new(),
                    },
                },
            ],
            vec![PromptProjectionItem {
                timestamp_ms: 0,
                activity_anchor: "evt-status".to_string(),
                role: "tool".to_string(),
                content: PromptProjectionContent::ToolResult {
                    tool_name: "system/discover_agents".to_string(),
                    result: json!({ "status": "sent" }),
                },
            }],
        ];
        for (i, items) in cases.into_iter().enumerate() {
            let history = project_prompt_context(items, &registry, &ref_table, None);
            assert_eq!(history, json!([]), "case {i}");
        }
    }

    #[test]
    fn repeated_send_done_emits_no_projection_rows() {
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
                        header: "@15 · \"found tasks\" · 209L · 6.3KB".to_string(),
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
                        header: "@15 · \"found tasks\" · 209L · 6.3KB".to_string(),
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
                        header: "@15 · \"found tasks\" · 209L · 6.3KB".to_string(),
                    },
                ),
            },
        ];

        let archive_reader =
            |archive_ref: &str, _grep: Option<&str>, _offset: usize, _limit: usize| {
                Some(format!("cat -n {archive_ref}\nTASK_LIST_PAYLOAD"))
            };

        let history = project_prompt_context(items, &registry, &ref_table, Some(&archive_reader));
        let rows = history.as_array().expect("array");
        assert!(
            rows.is_empty(),
            "SendDone steps must not allocate conversation_history rows: {history}"
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
    fn page_read_after_send_done_shows_archive_send_done_does_not() {
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
                        header: "@15 · \"found tasks\" · 209L · 6.3KB".to_string(),
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
            payload_occurrences, 1,
            "only PageRead may inline archive text; SendDone is not projected"
        );
    }

    #[test]
    fn tool_result_inline_preview_does_not_emit_archive_read_cta_when_truncated() {
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
            content.contains("more not shown in this preview"),
            "expected inline preview footer in: {content}"
        );
        assert!(
            !content.contains("SearchRead") && !content.contains("PageRead"),
            "inline preview must not instruct archive reads: {content}"
        );
        assert!(
            !content.contains("@N") && !content.contains("offset="),
            "inline preview must not invent archive refs or read offsets: {content}"
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
    }
}
