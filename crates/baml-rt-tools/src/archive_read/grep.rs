//! Grep + paginate over rendered archive lines.
//!
//! Behavior:
//! - No `grep`: return plain line pagination (`cat -n` style window).
//! - With `grep`: match candidate lines, then expand each match to a logical list-item block
//!   when possible (YAML-like `- ` entries by indentation), merge overlaps, and paginate the
//!   resulting contextual lines. This preserves nearby fields like id/name/url for status matches.

use std::collections::BTreeSet;

use super::{
    rendered::RenderedContent,
    types::{GrepPage, GrepPattern, LineOffset, LineWithPosition, PageLimit},
};

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ').count()
}

fn is_list_item_line(s: &str) -> bool {
    s.trim_start().starts_with("- ")
}

/// Try to expand a matched line to its surrounding YAML-like list item block.
/// Returns a half-open line-index range `[start, end)` in 0-based coordinates.
fn list_item_block_for_match(lines: &[&str], match_idx: usize) -> Option<(usize, usize)> {
    if lines.is_empty() || match_idx >= lines.len() {
        return None;
    }

    let match_indent = leading_spaces(lines[match_idx]);

    // Find nearest enclosing list item at indent <= match line.
    let mut start = None;
    for i in (0..=match_idx).rev() {
        let line = lines[i];
        if line.trim().is_empty() {
            continue;
        }
        let indent = leading_spaces(line);
        if is_list_item_line(line) && indent <= match_indent {
            start = Some(i);
            break;
        }
    }

    let start = start?;
    let item_indent = leading_spaces(lines[start]);

    // Walk until next sibling list item at same-or-less indent, or parent outdent.
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        let indent = leading_spaces(line);
        if (is_list_item_line(line) && indent <= item_indent) || indent < item_indent {
            end = i;
            break;
        }
    }

    Some((start, end))
}

/// Filter and paginate rendered content.
///
/// - Without grep: plain line pagination.
/// - With grep: block-aware grep pagination (context-preserving for list-style archives).
pub fn grep_paginate(
    content: &RenderedContent,
    grep: Option<&GrepPattern>,
    offset: LineOffset,
    limit: PageLimit,
) -> GrepPage {
    let lines: Vec<&str> = content.lines().collect();

    let matched: Vec<LineWithPosition> = match grep {
        None => lines
            .iter()
            .enumerate()
            .map(|(idx, line)| LineWithPosition {
                original_line_number: idx + 1,
                text: (*line).to_string(),
            })
            .collect(),
        Some(g) => {
            let match_indices: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| g.matches(line))
                .map(|(idx, _)| idx)
                .collect();

            // Expand match lines to list-item blocks when possible; fallback to single line.
            let mut include = BTreeSet::new();
            for idx in match_indices {
                if let Some((start, end)) = list_item_block_for_match(&lines, idx) {
                    for i in start..end {
                        include.insert(i);
                    }
                } else {
                    include.insert(idx);
                }
            }

            include
                .into_iter()
                .map(|idx| LineWithPosition {
                    original_line_number: idx + 1,
                    text: lines[idx].to_string(),
                })
                .collect()
        }
    };

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
    fn grep_expands_to_list_item_blocks() {
        let content = sample_content();
        let pattern = GrepPattern::parse("deploy").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
        // Matched lines (2 and 6) expand to two full list-item blocks: lines 1-2 and 5-6.
        assert_eq!(page.total_matched, 4);
        let got: Vec<usize> = page.lines.iter().map(|l| l.original_line_number).collect();
        assert_eq!(got, vec![1, 2, 5, 6]);
    }

    #[test]
    fn original_line_numbers_preserved_in_block_mode() {
        let content = sample_content();
        let pattern = GrepPattern::parse("alice").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
        assert_eq!(page.total_matched, 4);
        let got: Vec<usize> = page.lines.iter().map(|l| l.original_line_number).collect();
        assert_eq!(got, vec![1, 2, 5, 6]);
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
        let page = grep_paginate(&content, Some(&pattern), LineOffset(1), PageLimit::new(2));
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].original_line_number, 2);
        assert_eq!(page.lines[1].original_line_number, 5);
        assert!(page.has_more);
    }

    #[test]
    fn clickup_like_status_match_returns_full_item_block() {
        let content = RenderedContent::from_lines(vec![
            "tasks:".to_string(),
            "  - id: 86ag53uj5".to_string(),
            "    name: Task100".to_string(),
            "    status: to do".to_string(),
            "    url: https://app.clickup.com/t/86ag53uj5".to_string(),
            "    priority: high".to_string(),
            "  - id: 86xxxx".to_string(),
            "    name: Other".to_string(),
            "    status: in progress".to_string(),
            "    url: https://app.clickup.com/t/86xxxx".to_string(),
        ]);

        let pattern = GrepPattern::parse("status: to do").unwrap();
        let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));

        let got: Vec<&str> = page.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(got.contains(&"  - id: 86ag53uj5"));
        assert!(got.contains(&"    name: Task100"));
        assert!(got.contains(&"    status: to do"));
        assert!(got.contains(&"    url: https://app.clickup.com/t/86ag53uj5"));
        assert!(got.contains(&"    priority: high"));
    }
}
