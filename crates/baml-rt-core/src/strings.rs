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
}
