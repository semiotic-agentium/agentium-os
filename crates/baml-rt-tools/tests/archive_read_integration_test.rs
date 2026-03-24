//! Integration tests: render → grep → cat_n full pipeline.

use baml_rt_tools::archive_read::{
    GrepPattern, LineOffset, PageLimit, format_cat_n, grep_paginate, render_to_lines,
};
use serde_json::json;

#[test]
fn full_pipeline_slack_messages_grep_deploy() {
    let data = json!([
        {"ts": "14:01", "user": "alice", "text": "The deploy looks good to me"},
        {"ts": "14:02", "user": "bob", "text": "Can you check the auth module?"},
        {"ts": "14:05", "user": "alice", "text": "Merging to main now, deploy v2.1"},
        {"ts": "14:08", "user": "carol", "text": "I see a failing test in CI"},
        {"ts": "14:10", "user": "alice", "text": "Deploy v2.1 is green on staging"}
    ]);

    let content = render_to_lines(&data);
    assert!(
        content.line_count() > 5,
        "5 records should produce many YAML lines"
    );

    let pattern = GrepPattern::parse("-i deploy").unwrap();
    let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));

    assert!(
        page.total_matched >= 3,
        "at least 3 lines should match 'deploy' (case insensitive)"
    );

    let formatted = format_cat_n(&page.lines);
    for line in formatted.lines() {
        assert!(line.contains('\t'), "cat_n lines must have tab separator");
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        assert_eq!(parts.len(), 2, "should have number and content");
        let num: usize = parts[0]
            .trim()
            .parse()
            .expect("line number should be numeric");
        assert!(num >= 1, "line numbers are 1-based");
        let text = parts[1].to_lowercase();
        assert!(
            text.contains("deploy"),
            "grep-filtered line should contain 'deploy'"
        );
    }
}

#[test]
fn grep_preserves_original_positions_not_sequential() {
    let data = json!([
        {"id": 1, "msg": "alpha"},
        {"id": 2, "msg": "beta"},
        {"id": 3, "msg": "alpha again"},
        {"id": 4, "msg": "gamma"},
        {"id": 5, "msg": "alpha third"}
    ]);

    let content = render_to_lines(&data);
    let pattern = GrepPattern::parse("alpha").unwrap();
    let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));

    let line_nums: Vec<usize> = page.lines.iter().map(|l| l.original_line_number).collect();
    assert!(
        line_nums.windows(2).all(|w| w[0] < w[1]),
        "line numbers should be monotonically increasing: {line_nums:?}"
    );
    assert_ne!(
        line_nums,
        vec![1, 2, 3],
        "line numbers should NOT be sequentially renumbered"
    );
}

#[test]
fn ten_kb_string_grep_finds_content_in_block_scalar() {
    let huge = format!(
        "This is a normal start. {} And then a deployment error occurred at the end.",
        "x ".repeat(5000)
    );
    let data = json!({"log": huge});

    let content = render_to_lines(&data);
    assert!(
        content.line_count() > 50,
        "10KB string should wrap to many lines"
    );

    let pattern = GrepPattern::parse("deployment error").unwrap();
    let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
    assert!(
        page.total_matched >= 1,
        "grep should find 'deployment error' in wrapped block scalar"
    );
}

#[test]
fn regex_grep_pipeline() {
    let data = json!([
        {"level": "INFO", "msg": "server started"},
        {"level": "ERROR", "msg": "connection refused"},
        {"level": "WARN", "msg": "high latency detected"},
        {"level": "INFO", "msg": "request completed"},
        {"level": "ERROR", "msg": "timeout"}
    ]);

    let content = render_to_lines(&data);
    let pattern = GrepPattern::parse("-E ERROR|WARN").unwrap();
    let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
    assert!(page.total_matched >= 3, "should match ERROR and WARN lines");
}

#[test]
fn case_insensitive_fixed_grep() {
    let data = json!([
        {"name": "Alice"},
        {"name": "bob"},
        {"name": "ALICE_ADMIN"}
    ]);

    let content = render_to_lines(&data);
    let pattern = GrepPattern::parse("-i alice").unwrap();
    let page = grep_paginate(&content, Some(&pattern), LineOffset(0), PageLimit::new(100));
    assert_eq!(
        page.total_matched, 2,
        "case insensitive should match Alice and ALICE_ADMIN"
    );
}

#[test]
fn pagination_with_has_more() {
    let data = json!([
        {"i": 1}, {"i": 2}, {"i": 3}, {"i": 4}, {"i": 5},
        {"i": 6}, {"i": 7}, {"i": 8}, {"i": 9}, {"i": 10}
    ]);

    let content = render_to_lines(&data);
    let page1 = grep_paginate(&content, None, LineOffset(0), PageLimit::new(5));
    assert_eq!(page1.lines.len(), 5);
    assert!(page1.has_more);

    let page2 = grep_paginate(&content, None, LineOffset(5), PageLimit::new(5));
    assert!(page2.lines.len() <= 5);
}
