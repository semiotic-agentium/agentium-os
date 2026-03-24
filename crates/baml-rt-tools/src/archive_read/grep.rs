//! Line-level grep + paginate.
//!
//! Pure `grep | tail -n +OFFSET | head -n LIMIT` semantics.
//! No record awareness. Line numbers are original positions in
//! the full content, preserved through filtering.

use super::{
    rendered::RenderedContent,
    types::{GrepPage, GrepPattern, LineOffset, LineWithPosition, PageLimit},
};

/// Filter and paginate rendered content.
///
/// 1. If `grep` is Some, keep only lines matching the pattern.
/// 2. Skip `offset` matched lines.
/// 3. Return up to `limit` matched lines with their original 1-based positions.
pub fn grep_paginate(
    content: &RenderedContent,
    grep: Option<&GrepPattern>,
    offset: LineOffset,
    limit: PageLimit,
) -> GrepPage {
    let matched: Vec<LineWithPosition> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| grep.is_none_or(|g| g.matches(line)))
        .map(|(idx, line)| LineWithPosition {
            original_line_number: idx + 1,
            text: line.to_string(),
        })
        .collect();

    let total_matched = matched.len();
    let start = offset.0.min(total_matched);
    let end = (start + limit.get()).min(total_matched);
    let page = matched[start..end].to_vec();
    let has_more = end < total_matched;

    GrepPage {
        lines: page,
        total_matched,
        has_more,
        next_offset: end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_content() -> RenderedContent {
        RenderedContent::from_lines(vec![
            "- user: alice".to_string(),
            "  text: the deploy pipeline is broken".to_string(),
            "- user: bob".to_string(),
            "  text: I fixed the auth issue".to_string(),
            "- user: alice".to_string(),
            "  text: deploy v2.1 is green on staging".to_string(),
        ])
    }

    #[test]
    fn no_grep_returns_all() {
        let content = sample_content();
        let page = grep_paginate(&content, None, LineOffset(0), PageLimit::new(100));
        assert_eq!(page.total_matched, 6);
        assert_eq!(page.lines.len(), 6);
        assert!(!page.has_more);
    }

    #[test]
    fn grep_filters_lines() {
        let content = sample_content();
        let pattern = GrepPattern::parse("deploy").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
        assert_eq!(page.total_matched, 2);
        assert_eq!(page.lines[0].original_line_number, 2);
        assert_eq!(page.lines[1].original_line_number, 6);
    }

    #[test]
    fn original_line_numbers_preserved() {
        let content = sample_content();
        let pattern = GrepPattern::parse("alice").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
        assert_eq!(page.total_matched, 2);
        assert_eq!(page.lines[0].original_line_number, 1);
        assert_eq!(page.lines[1].original_line_number, 5);
    }

    #[test]
    fn offset_skips_matched_lines() {
        let content = sample_content();
        let page = grep_paginate(&content, None, LineOffset(2), PageLimit::new(2));
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].original_line_number, 3);
        assert_eq!(page.lines[1].original_line_number, 4);
        assert!(page.has_more);
    }

    #[test]
    fn limit_caps_results() {
        let content = sample_content();
        let page = grep_paginate(&content, None, LineOffset(0), PageLimit::new(3));
        assert_eq!(page.lines.len(), 3);
        assert!(page.has_more);
        assert_eq!(page.total_matched, 6);
    }

    #[test]
    fn offset_beyond_end() {
        let content = sample_content();
        let page = grep_paginate(&content, None, LineOffset(100), PageLimit::new(20));
        assert!(page.lines.is_empty());
        assert!(!page.has_more);
    }

    #[test]
    fn grep_with_offset_and_limit() {
        let content = sample_content();
        let pattern = GrepPattern::parse("alice").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(1), PageLimit::new(10));
        assert_eq!(page.lines.len(), 1);
        assert_eq!(page.lines[0].original_line_number, 5);
        assert!(!page.has_more);
    }
}
