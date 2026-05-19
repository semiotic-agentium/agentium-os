//! Locate top-level `function NAME(args) -> RET { ... }` declarations in a BAML source file.
//!
//! The scanner is comment- and string-aware via [`super::lexer::scan_trivia`]. Bracket counting
//! always asks the lexer to skip over trivia regions before consuming a brace, so braces inside
//! Jinja templates, comments, or string literals never confuse the parser.

use super::lexer::{is_ident_byte, is_whitespace, scan_trivia};

/// One parsed `function NAME(...) -> RET { ... }` declaration.
#[derive(Debug, Clone)]
pub(super) struct FunctionDecl {
    pub fn_name: String,
    /// Byte index of the `{` that opens the function body. Inclusive (the brace is included
    /// in the returned range so callers can splice the original header bytes verbatim).
    pub body_open_brace_inclusive: usize,
    /// Byte index just past the matching `}` that closes the function body.
    pub body_close_brace_exclusive: usize,
}

/// Try to parse a function declaration starting at byte offset `i`. Returns `None` when `i`
/// does not begin with the `function` keyword followed by an identifier and well-formed
/// `(...) -> RET { ... }`.
pub(super) fn parse_function_declaration(bytes: &[u8], i: usize) -> Option<FunctionDecl> {
    const KEYWORD: &[u8] = b"function";
    if !starts_with_token(bytes, i, KEYWORD) {
        return None;
    }
    let after = i + KEYWORD.len();
    let mut p = skip_whitespace(bytes, after);

    let ident_start = p;
    while p < bytes.len() && is_ident_byte(bytes[p]) {
        p += 1;
    }
    if p == ident_start {
        return None;
    }
    let fn_name = std::str::from_utf8(&bytes[ident_start..p])
        .ok()?
        .to_string();

    p = skip_whitespace(bytes, p);
    if p >= bytes.len() || bytes[p] != b'(' {
        return None;
    }
    p = skip_balanced_brackets(bytes, p, b'(', b')')?;

    p = skip_whitespace(bytes, p);
    if p + 1 >= bytes.len() || bytes[p] != b'-' || bytes[p + 1] != b'>' {
        return None;
    }
    p += 2;

    while p < bytes.len() && bytes[p] != b'{' {
        if let Some(span) = scan_trivia(bytes, p) {
            p = span.end;
            continue;
        }
        p += 1;
    }
    if p >= bytes.len() {
        return None;
    }
    let body_open = p;
    let body_close = skip_balanced_brackets(bytes, body_open, b'{', b'}')?;

    Some(FunctionDecl {
        fn_name,
        body_open_brace_inclusive: body_open,
        body_close_brace_exclusive: body_close,
    })
}

/// True when `bytes[i..]` starts with `keyword` and the position respects identifier word
/// boundaries on both sides (no leading or trailing identifier byte).
fn starts_with_token(bytes: &[u8], i: usize, keyword: &[u8]) -> bool {
    if i + keyword.len() > bytes.len() {
        return false;
    }
    if &bytes[i..i + keyword.len()] != keyword {
        return false;
    }
    if i > 0 && is_ident_byte(bytes[i - 1]) {
        return false;
    }
    let after = i + keyword.len();
    if after >= bytes.len() {
        return false;
    }
    is_whitespace(bytes[after])
}

fn skip_whitespace(bytes: &[u8], i: usize) -> usize {
    let mut p = i;
    while p < bytes.len() && is_whitespace(bytes[p]) {
        p += 1;
    }
    p
}

/// Bracket-aware scan that respects [`scan_trivia`]: braces inside comments / strings /
/// `prompt #"..."#` raw strings are not counted toward the depth balance.
fn skip_balanced_brackets(bytes: &[u8], i: usize, open: u8, close: u8) -> Option<usize> {
    if bytes.get(i) != Some(&open) {
        return None;
    }
    let mut depth = 0i32;
    let mut p = i;
    while p < bytes.len() {
        if let Some(span) = scan_trivia(bytes, p) {
            p = span.end;
            continue;
        }
        let b = bytes[p];
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(p + 1);
            }
        }
        p += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_function() {
        let src = "function F(x: string) -> string { client C\n  prompt #\"a\"#\n}";
        let decl = parse_function_declaration(src.as_bytes(), 0).expect("parsed");
        assert_eq!(decl.fn_name, "F");
        assert_eq!(
            &src[decl.body_open_brace_inclusive..=decl.body_open_brace_inclusive],
            "{"
        );
        assert_eq!(
            &src[decl.body_close_brace_exclusive - 1..decl.body_close_brace_exclusive],
            "}"
        );
    }

    #[test]
    fn handles_braces_inside_raw_string_body() {
        let src = "function F(x: string) -> string { client C\n  prompt #\"  {{ x }}\"#\n}";
        let decl = parse_function_declaration(src.as_bytes(), 0).expect("parsed");
        assert_eq!(decl.fn_name, "F");
        assert_eq!(decl.body_close_brace_exclusive, src.len());
    }

    #[test]
    fn rejects_words_starting_with_function() {
        let src = "functional thing";
        assert!(parse_function_declaration(src.as_bytes(), 0).is_none());
    }

    #[test]
    fn rejects_function_in_class_body_at_offset() {
        let src = "class Foo {} function F(x: string) -> string { }";
        let pos = src.find("function").expect("found");
        let decl = parse_function_declaration(src.as_bytes(), pos).expect("parsed");
        assert_eq!(decl.fn_name, "F");
    }

    #[test]
    fn nested_braces_in_args_block() {
        let src = "function F(x: map<string, int>) -> string { client C }";
        let decl = parse_function_declaration(src.as_bytes(), 0).expect("parsed");
        assert_eq!(decl.fn_name, "F");
    }
}
