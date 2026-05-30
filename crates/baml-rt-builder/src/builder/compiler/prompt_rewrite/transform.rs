// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Replace the `prompt #"..."#` body inside one parsed function declaration with the canonical
//! structured skeleton emitted by [`PromptCompositor::authored_non_fsm`].
//!
//! The transformer is the only place that knows how to splice rewritten bytes back into source.
//! It locates the prompt literal via the trivia-aware lexer (so `prompt` keywords inside
//! comments / strings are ignored) and replaces only the inner content of the raw string.

use super::lexer::{is_ident_byte, is_whitespace, scan_raw_string_inner, scan_trivia};
use crate::builder::baml_gen::PromptCompositor;

/// Range of one `prompt #+ "..." #+` literal inside a function body.
#[derive(Debug, Clone, Copy)]
struct PromptLiteralSpan {
    inner_start: usize,
    inner_end: usize,
}

/// Outcome of attempting to rewrite a single function body.
#[derive(Debug, Clone)]
pub(super) enum BodyTransformOutcome {
    Rewritten(String),
    NoPromptLiteral,
}

/// Locate the function's `prompt #"..."#` literal and replace its inner content with the
/// canonical authored-non-FSM skeleton via [`PromptCompositor::authored_non_fsm`].
pub(super) fn transform_function_body(body: &str, selection_hint: &str) -> BodyTransformOutcome {
    let bytes = body.as_bytes();
    let Some(span) = locate_prompt_literal(bytes) else {
        return BodyTransformOutcome::NoPromptLiteral;
    };
    let inner = &body[span.inner_start..span.inner_end];
    let rewritten_inner = PromptCompositor::authored_non_fsm(inner, selection_hint);

    let mut out = String::with_capacity(body.len() + rewritten_inner.len());
    out.push_str(&body[..span.inner_start]);
    out.push_str(&rewritten_inner);
    out.push_str(&body[span.inner_end..]);
    BodyTransformOutcome::Rewritten(out)
}

fn locate_prompt_literal(bytes: &[u8]) -> Option<PromptLiteralSpan> {
    const KEYWORD: &[u8] = b"prompt";
    let mut i = 0;
    while i < bytes.len() {
        if let Some(span) = scan_trivia(bytes, i) {
            i = span.end;
            continue;
        }
        if is_prompt_keyword_at(bytes, i, KEYWORD) {
            let mut p = i + KEYWORD.len();
            while p < bytes.len() && is_whitespace(bytes[p]) {
                p += 1;
            }
            if p < bytes.len()
                && bytes[p] == b'#'
                && let Some(raw) = scan_raw_string_inner(bytes, p)
            {
                return Some(PromptLiteralSpan {
                    inner_start: raw.inner_start,
                    inner_end: raw.inner_end,
                });
            }
            i = p;
            continue;
        }
        i += 1;
    }
    None
}

fn is_prompt_keyword_at(bytes: &[u8], i: usize, keyword: &[u8]) -> bool {
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
    after < bytes.len() && is_whitespace(bytes[after])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_prompt_inner_only() {
        let body = "{ client C\n  prompt #\"\n    Hello.\n  \"#\n}";
        match transform_function_body(
            body,
            "Return exactly one output matching the schema below.\n",
        ) {
            BodyTransformOutcome::Rewritten(out) => {
                assert!(out.contains("Hello."));
                assert!(out.contains("ctx.tags['tool_schema_prelude']"));
                assert!(out.contains("Session history:"));
                assert!(out.contains("Return exactly one output matching the schema below."));
                assert_eq!(out.matches("{{ ctx.output_format }}").count(), 1);
                // Header bytes preserved.
                assert!(out.starts_with("{ client C\n  prompt #\""));
                assert!(out.ends_with("\"#\n}"));
            }
            BodyTransformOutcome::NoPromptLiteral => panic!("expected rewrite"),
        }
    }

    #[test]
    fn returns_no_prompt_literal_for_bodyless_function() {
        let body = "{ client C\n}";
        assert!(matches!(
            transform_function_body(
                body,
                "Return exactly one output matching the schema below.\n"
            ),
            BodyTransformOutcome::NoPromptLiteral
        ));
    }

    #[test]
    fn ignores_word_promptly() {
        let body = "{ client C\n  // promptly do thing\n}";
        assert!(matches!(
            transform_function_body(
                body,
                "Return exactly one output matching the schema below.\n"
            ),
            BodyTransformOutcome::NoPromptLiteral
        ));
    }
}
