//! Session-shaped archive read text: synthetic `cat -n` / `grep -n` command, optional range
//! comment, then `format_cat_n` numbered lines. Shared by prompt projection, episode assembly,
//! and provenance read-replay hydration.

use serde_json::Value;

use super::{
    cat_n::format_cat_n,
    grep::grep_paginate,
    render::render_to_lines,
    rendered::RenderedContent,
    types::{GrepPage, GrepPattern, LineOffset, PageLimit, ShortRef},
};
use crate::archive_refs::RefTable;

/// A rendered archive page with the concrete ref required to tell the model how to continue.
pub struct ArchiveReadPage<'a> {
    pub page: &'a GrepPage,
    pub archive_ref: &'a str,
}

impl ArchiveReadPage<'_> {
    /// Host instruction on the line after the synthetic `cat -n` / `grep -n` command, before
    /// numbered lines. Rendering this instruction requires a concrete archive ref.
    #[must_use]
    pub fn session_range_comment(&self) -> String {
        if self.page.lines.is_empty() {
            return String::new();
        }
        let first = self
            .page
            .lines
            .first()
            .map(|l| l.original_line_number)
            .unwrap_or(1);
        let last = self
            .page
            .lines
            .last()
            .map(|l| l.original_line_number)
            .unwrap_or(1);
        if self.page.has_more {
            let remaining = self
                .page
                .total_matched
                .saturating_sub(self.page.next_offset);
            let off = self.page.next_offset;
            let archive_ref = self.archive_ref;
            format!(
                "  # Window: lines {first}-{last} of {total}. More lines are available ({rem} more; next offset={off}). If additional evidence is needed, use SearchRead (non-empty grep) to narrow {archive_ref}, or PageRead {archive_ref} with offset={off}.",
                total = self.page.total_matched,
                rem = remaining,
                off = off,
            )
        } else if first == 1 && last == self.page.total_matched {
            String::new()
        } else {
            format!("  # lines {first}-{last} of {}", self.page.total_matched)
        }
    }
}

/// CLI analogue for a session `Read` when showing the command line only (no resolved archive).
#[must_use]
pub fn session_read_command_line(archive_ref_str: &str, grep_pattern_raw: Option<&str>) -> String {
    match grep_pattern_raw.filter(|s| !s.is_empty()) {
        Some(pat) => format!("grep -n '{pat}' {archive_ref_str}"),
        None => format!("cat -n {archive_ref_str}"),
    }
}

/// Format a single grep page as the multi-line block injected into session history / transcripts.
#[must_use]
pub fn format_grep_page_as_session_read_body(
    page: &GrepPage,
    archive_ref_str: &str,
    grep_pattern_raw: Option<&str>,
) -> String {
    let formatted = format_cat_n(&page.lines);
    let cmd = session_read_command_line(archive_ref_str, grep_pattern_raw);
    if page.lines.is_empty() {
        return format!("{cmd}\n# no matches");
    }
    let range_comment = ArchiveReadPage {
        page,
        archive_ref: archive_ref_str,
    }
    .session_range_comment();
    // `str::lines` + `join("\n")` drops a final newline; keep one canonical form for transcript
    // ToolOutput lines and session_history string content. Put the synthetic command on its
    // own line; when a range or paging line exists, it follows on the next line (avoids
    // `]cat -n@N# lines…` when history rows are concatenated without extra separators).
    let body = if range_comment.is_empty() {
        format!("{cmd}\n{formatted}")
    } else {
        format!("{cmd}\n{range_comment}\n{formatted}")
    };
    body.trim_end_matches('\n').to_string()
}

/// Paginate rendered archive lines and format as session read body (grep parse + `grep_paginate` +
/// [`format_grep_page_as_session_read_body`]).
#[must_use]
pub fn format_session_read_body_from_rendered(
    rendered: &RenderedContent,
    archive_ref: &str,
    grep_raw: Option<&str>,
    offset: usize,
    page_limit: PageLimit,
) -> String {
    let grep = grep_raw
        .filter(|s| !s.is_empty())
        .and_then(|s| GrepPattern::parse(s).ok());
    let page = grep_paginate(rendered, grep.as_ref(), LineOffset(offset), page_limit);
    format_grep_page_as_session_read_body(&page, archive_ref, grep_raw)
}

/// JSON → render → paginate → session read body. Returns [`None`] when there is no rendered
/// content (empty [`RenderedContent`]).
#[must_use]
pub fn format_session_read_body_from_json_value(
    value: &Value,
    archive_ref: &str,
    grep_raw: Option<&str>,
    offset: usize,
    page_limit: PageLimit,
) -> Option<String> {
    let rendered = render_to_lines(value);
    if rendered.is_empty() {
        return None;
    }
    Some(format_session_read_body_from_rendered(
        &rendered,
        archive_ref,
        grep_raw,
        offset,
        page_limit,
    ))
}

/// Look up `archive_ref_str` in a graph [`RefTable`], then format the session read body the same
/// way as a prompt [`ArchiveReader`](crate::prompt_projection::ArchiveReader) closure. Shared by
/// `baml-rt-conversation`’s `assemble_session_history` and the episode reader in
/// `baml-rt-provenance`.
#[must_use]
pub fn format_session_read_from_vtable(
    vtable: &RefTable,
    archive_ref_str: &str,
    grep_str: Option<&str>,
    offset: usize,
    limit: usize,
) -> Option<String> {
    let short_ref = ShortRef::parse_loose(archive_ref_str)?;
    let entry = vtable.get(short_ref)?;
    Some(format_session_read_body_from_rendered(
        &entry.content,
        archive_ref_str,
        grep_str,
        offset,
        PageLimit::new(limit),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        super::{rendered::RenderedContent, types::PageLimit},
        *,
    };

    #[test]
    fn session_read_command_line_matches_cat_and_grep() {
        assert_eq!(session_read_command_line("@3", None), "cat -n @3");
        assert_eq!(
            session_read_command_line("@3", Some("foo")),
            "grep -n 'foo' @3"
        );
        assert_eq!(session_read_command_line("@3", Some("")), "cat -n @3");
    }

    #[test]
    fn format_session_read_body_from_rendered_round_trip_lines() {
        let rendered = RenderedContent::from_lines(vec!["a: 1".to_string(), "b: 2".to_string()]);
        let s =
            format_session_read_body_from_rendered(&rendered, "@1", None, 0, PageLimit::new(50));
        assert!(s.starts_with("cat -n @1\n"));
        assert!(s.contains("1\ta: 1"));
        assert!(s.contains("2\tb: 2"));
        assert!(!s.ends_with('\n'));
    }

    #[test]
    fn format_grep_page_puts_non_imperative_paging_hint_after_command() {
        let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        let rendered = RenderedContent::from_lines(lines);
        let s = format_session_read_body_from_rendered(&rendered, "@1", None, 0, PageLimit::new(2));
        let lines: Vec<_> = s.split('\n').collect();
        assert_eq!(lines[0], "cat -n @1", "synthetic command on its own line");
        assert!(
            lines[1].contains("More lines are available")
                && lines[1].contains("next offset=2")
                && lines[1].contains("If additional evidence is needed"),
            "non-imperative paging hint after command, got: {:?}",
            lines[1]
        );
        assert!(
            lines[2].ends_with("line 0") && lines[2].contains('\t'),
            "numbered content after paging, got: {:?}",
            lines[2]
        );
    }

    #[test]
    fn archive_read_page_session_range_comment_when_truncated_names_ref() {
        let lines: Vec<String> = (0..100).map(|i| format!("L{i}")).collect();
        let rendered = RenderedContent::from_lines(lines);
        let page = grep_paginate(&rendered, None, LineOffset::default(), PageLimit::new(40));
        assert!(page.has_more);

        let c = ArchiveReadPage {
            page: &page,
            archive_ref: "@7",
        }
        .session_range_comment();

        assert!(c.contains("@7"), "{c}");
        assert!(c.contains("offset=40"), "{c}");
        assert!(c.contains("60 more"), "{c}");
        assert!(c.contains("SearchRead"), "{c}");
        assert!(c.contains("PageRead"), "{c}");
        assert!(!c.contains("this @N"), "{c}");
    }
}
