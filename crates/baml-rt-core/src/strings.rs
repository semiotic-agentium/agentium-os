//! Shared string-processing utilities.

/// Truncate `input` to at most `max_chars` **characters** (not bytes).
///
/// If truncation occurs the result is capped at `max_chars` characters
/// followed by `"..."`.  Inputs that fit within the limit are returned
/// unchanged.
pub fn truncate_chars_with_ellipsis(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars) + 3);
    for (i, ch) in input.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

/// Trim whitespace then truncate an owned [`String`] in place.
///
/// Empty or whitespace-only strings are left as empty.  Otherwise the
/// string is trimmed and, if it exceeds `max_chars` characters, truncated
/// with an ellipsis via [`truncate_chars_with_ellipsis`].
pub fn trim_and_truncate(text: &mut String, max_chars: usize) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if trimmed.chars().count() > max_chars {
        *text = truncate_chars_with_ellipsis(trimmed, max_chars);
    } else if trimmed.len() != text.len() {
        *text = trimmed.to_string();
    }
}

/// Like [`trim_and_truncate`] but for `Option<String>`.
///
/// `None` and whitespace-only values become `None`.
pub fn trim_and_truncate_option(text: &mut Option<String>, max_chars: usize) {
    let Some(current) = text.as_ref() else {
        return;
    };
    let trimmed = current.trim();
    if trimmed.is_empty() {
        *text = None;
        return;
    }
    if trimmed.chars().count() > max_chars {
        *text = Some(truncate_chars_with_ellipsis(trimmed, max_chars));
    } else if trimmed.len() != current.len() {
        *text = Some(trimmed.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_below_limit_unchanged() {
        assert_eq!(truncate_chars_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_limit_unchanged() {
        assert_eq!(truncate_chars_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_above_limit_adds_ellipsis() {
        assert_eq!(truncate_chars_with_ellipsis("hello world", 5), "hello...");
    }

    #[test]
    fn truncate_multi_byte_chars() {
        // 3 chars, limit 2 → keeps 2 chars + "..."
        assert_eq!(truncate_chars_with_ellipsis("héllo", 2), "hé...");
    }

    // -----------------------------------------------------------------------
    // trim_and_truncate
    // -----------------------------------------------------------------------

    #[test]
    fn trim_and_truncate_empty_is_noop() {
        let mut val = String::new();
        trim_and_truncate(&mut val, 10);
        assert_eq!(val, "");
    }

    #[test]
    fn trim_and_truncate_within_limit_unchanged() {
        let mut val = "short".to_string();
        trim_and_truncate(&mut val, 10);
        assert_eq!(val, "short");
    }

    #[test]
    fn trim_and_truncate_at_boundary_unchanged() {
        let mut val = "12345".to_string();
        trim_and_truncate(&mut val, 5);
        assert_eq!(val, "12345");
    }

    #[test]
    fn trim_and_truncate_over_boundary_truncates() {
        let mut val = "x".repeat(400);
        trim_and_truncate(&mut val, 10);
        assert!(val.ends_with("..."));
        assert_eq!(val.chars().count(), 13);
    }

    #[test]
    fn trim_and_truncate_multi_byte() {
        let mut val = "ñ".repeat(20);
        trim_and_truncate(&mut val, 5);
        assert!(val.ends_with("..."));
        assert_eq!(val.chars().count(), 8); // 5 + "..."
    }

    // -----------------------------------------------------------------------
    // trim_and_truncate_option
    // -----------------------------------------------------------------------

    #[test]
    fn trim_and_truncate_option_none_is_noop() {
        let mut val: Option<String> = None;
        trim_and_truncate_option(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn trim_and_truncate_option_empty_becomes_none() {
        let mut val = Some(String::new());
        trim_and_truncate_option(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn trim_and_truncate_option_whitespace_becomes_none() {
        let mut val = Some("   ".to_string());
        trim_and_truncate_option(&mut val, 10);
        assert!(val.is_none());
    }

    #[test]
    fn trim_and_truncate_option_within_limit_unchanged() {
        let mut val = Some("short".to_string());
        trim_and_truncate_option(&mut val, 10);
        assert_eq!(val.as_deref(), Some("short"));
    }

    #[test]
    fn trim_and_truncate_option_over_limit_truncates() {
        let mut val = Some("a]".repeat(150));
        trim_and_truncate_option(&mut val, 10);
        let result = val.unwrap();
        assert!(result.ends_with("..."));
        // 10 chars + "..."
        assert_eq!(result.chars().count(), 13);
    }
}
