//! Session-shaped archive read text: synthetic `cat -n` / `grep -n` command, optional range
//! comment, then `format_cat_n` numbered lines. Shared by prompt projection, episode assembly,
//! and provenance read-replay hydration.

use serde_json::Value;

use super::{
    cat_n::format_cat_n,
    grep::grep_paginate,
    render::render_to_lines,
    rendered::RenderedContent,
    types::{GrepPage, GrepPattern, LineOffset, PageLimit},
};

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
    let range_comment = page.session_range_comment();
    // `str::lines` + `join("\n")` drops a final newline; keep one canonical form for transcript
    // ToolOutput lines and session_history string content.
    format!("{cmd}{range_comment}\n{formatted}")
        .trim_end_matches('\n')
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::{super::rendered::RenderedContent, *};

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
}
