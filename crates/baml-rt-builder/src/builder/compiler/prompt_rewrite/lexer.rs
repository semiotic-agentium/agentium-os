// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conservative byte-level lexer that recognises BAML's "skippable" trivia (line + block
//! comments and quoted/raw strings). Production rule: when the lexer reports a region, callers
//! advance past it without inspecting interior bytes — this keeps brace/paren/`prompt`-keyword
//! detection safe inside string literals, comments, and Jinja templates that look like braces.
//!
//! The lexer is intentionally narrow: it does **not** know BAML's full grammar. It only knows
//! enough to step over byte ranges that must not be treated as code.

/// One contiguous region of trivia (comment / string literal) the parser must skip over.
#[derive(Debug, Clone, Copy)]
pub(super) struct TriviaSpan {
    /// Byte index just past the trivia. The caller continues scanning from here.
    pub end: usize,
}

/// Try to recognise trivia at `i`. Returns `Some(span)` whose `span.end` lies past the trivia,
/// or `None` if `bytes[i]` is regular code.
pub(super) fn scan_trivia(bytes: &[u8], i: usize) -> Option<TriviaSpan> {
    if i >= bytes.len() {
        return None;
    }
    if let Some(end) = scan_line_comment(bytes, i) {
        return Some(TriviaSpan { end });
    }
    if let Some(end) = scan_block_comment(bytes, i) {
        return Some(TriviaSpan { end });
    }
    if let Some(end) = scan_raw_string(bytes, i) {
        return Some(TriviaSpan { end });
    }
    if let Some(end) = scan_double_quoted_string(bytes, i) {
        return Some(TriviaSpan { end });
    }
    None
}

fn scan_line_comment(bytes: &[u8], i: usize) -> Option<usize> {
    if i + 1 >= bytes.len() || bytes[i] != b'/' || bytes[i + 1] != b'/' {
        return None;
    }
    let mut j = i + 2;
    while j < bytes.len() && bytes[j] != b'\n' {
        j += 1;
    }
    Some(j)
}

fn scan_block_comment(bytes: &[u8], i: usize) -> Option<usize> {
    if i + 1 >= bytes.len() || bytes[i] != b'/' || bytes[i + 1] != b'*' {
        return None;
    }
    let mut j = i + 2;
    while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
        j += 1;
    }
    Some((j + 2).min(bytes.len()))
}

fn scan_double_quoted_string(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes[i] != b'"' {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && bytes[j] != b'"' {
        if bytes[j] == b'\\' && j + 1 < bytes.len() {
            j += 2;
            continue;
        }
        j += 1;
    }
    Some((j + 1).min(bytes.len()))
}

/// Recognise a BAML raw string literal (`#+ "..." #+` with matching `#` count). Returns the
/// index past the closing fence on success.
pub(super) fn scan_raw_string(bytes: &[u8], i: usize) -> Option<usize> {
    let inner = scan_raw_string_inner(bytes, i)?;
    Some(inner.end_after_close_fence)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RawStringSpan {
    /// Byte index of the first content byte after the opening `"`.
    pub inner_start: usize,
    /// Byte index of the closing `"` (i.e. exclusive end of the inner content).
    pub inner_end: usize,
    /// Byte index just past the closing `#` fence.
    pub end_after_close_fence: usize,
}

/// Like [`scan_raw_string`] but returns the inner-content range as well. Used by the prompt
/// literal locator to splice a rewritten body back into the source.
pub(super) fn scan_raw_string_inner(bytes: &[u8], i: usize) -> Option<RawStringSpan> {
    if i >= bytes.len() || bytes[i] != b'#' {
        return None;
    }
    let mut hashes = 0usize;
    while i + hashes < bytes.len() && bytes[i + hashes] == b'#' {
        hashes += 1;
    }
    if hashes == 0 || i + hashes >= bytes.len() || bytes[i + hashes] != b'"' {
        return None;
    }
    let inner_start = i + hashes + 1;
    let mut j = inner_start;
    while j < bytes.len() {
        if bytes[j] == b'"' {
            let mut close = 0usize;
            while close < hashes && j + 1 + close < bytes.len() && bytes[j + 1 + close] == b'#' {
                close += 1;
            }
            if close == hashes {
                return Some(RawStringSpan {
                    inner_start,
                    inner_end: j,
                    end_after_close_fence: j + 1 + hashes,
                });
            }
        }
        j += 1;
    }
    None
}

#[inline]
pub(super) fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

#[inline]
pub(super) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_line_comment() {
        let s = b"// hi\nrest";
        let span = scan_trivia(s, 0).expect("trivia");
        assert_eq!(span.end, 5);
    }

    #[test]
    fn skips_block_comment() {
        let s = b"/* a */ rest";
        let span = scan_trivia(s, 0).expect("trivia");
        assert_eq!(span.end, 7);
    }

    #[test]
    fn skips_double_quoted_string() {
        let s = br#""abc" rest"#;
        let span = scan_trivia(s, 0).expect("trivia");
        assert_eq!(span.end, 5);
    }

    #[test]
    fn skips_double_quoted_string_with_escape() {
        let s = br#""a\"b" rest"#;
        let span = scan_trivia(s, 0).expect("trivia");
        assert_eq!(span.end, 6);
    }

    #[test]
    fn skips_raw_string_single_hash() {
        let s = br##"#"abc"# rest"##;
        let span = scan_trivia(s, 0).expect("trivia");
        assert_eq!(span.end, 7);
    }

    #[test]
    fn raw_string_inner_returns_content_range() {
        let s = br##"#"hello"# rest"##;
        let inner = scan_raw_string_inner(s, 0).expect("raw");
        assert_eq!(inner.inner_start, 2);
        assert_eq!(inner.inner_end, 7);
        assert_eq!(inner.end_after_close_fence, 9);
    }

    #[test]
    fn returns_none_for_regular_code() {
        let s = b"function";
        assert!(scan_trivia(s, 0).is_none());
    }
}
