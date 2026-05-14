//! Idempotent author-body sanitizer used by both the authored-prompt rewriter and the
//! phase-executor template stripper.
//!
//! Authored BAML files routinely place `{{ ctx.output_format }}`, conversation-transcript Jinja
//! blocks, or already-injected `ctx.tags['tool_schema_prelude']` references in the middle of a
//! prompt. The compositor needs that text removed before it composes the canonical structure;
//! likewise, generated phase executors need IR text stripped of legacy `Phase: SELECT|ACT|CONTINUE`
//! cues and `[OPEN]`/`[ACT]`/`[CONTINUE]` bracket lines.
//!
//! All strip helpers are pure string transformations and **idempotent**: running them twice
//! produces the same output. That guarantees the rewriter is safe to re-run on already-rewritten
//! files (e.g. when an agent rebuilds without `regen-fixtures`).

use std::sync::LazyLock;

use baml_rt_tools::SESSION_STEP_STABLE_PREFIX_BAML;
use regex::Regex;

const AUTHORED_SELECTION_HINT: &str = "Return exactly one output matching the schema below. Do not add extra text before or after it.";
const LEGACY_ARCHIVE_HANDLE_GUIDANCE: &str = "Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.";
const LEGACY_OPEN_FIELD_GUIDANCE: &str = "For `op: \"Open\"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.";

static EXTRA_BLANK_RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("EXTRA_BLANK_RUNS: fixed \n{3,} pattern"));

static TRANSCRIPT_BLOCK_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)\{%\s*if\s+ctx\.tags\[\s*(?:'conversation_transcript'|"conversation_transcript")\s*\]\s*%\}.*?\{%\s*endif\s*%\}"#,
    )
    .expect("TRANSCRIPT_BLOCK_BRACKET: fixed transcript-if regex")
});

static TRANSCRIPT_BLOCK_DOT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)\{%\s*if\s+ctx\.tags\.conversation_transcript\s*%\}.*?\{%\s*endif\s*%\}")
        .expect("TRANSCRIPT_BLOCK_DOT: fixed transcript-if regex")
});

static PRELUDE_BLOCK_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)\{%\s*if\s+ctx\.tags\[\s*(?:'tool_schema_prelude'|"tool_schema_prelude")\s*\]\s*%\}.*?\{%\s*endif\s*%\}"#,
    )
    .expect("PRELUDE_BLOCK_BRACKET: fixed prelude-if regex")
});

static PRELUDE_BLOCK_DOT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)\{%\s*if\s+ctx\.tags\.tool_schema_prelude\s*%\}.*?\{%\s*endif\s*%\}")
        .expect("PRELUDE_BLOCK_DOT: fixed prelude-if regex")
});

static LEGACY_PHASE_CUE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*Phase:\s*(SELECT|ACT|CONTINUE)\b")
        .expect("LEGACY_PHASE_CUE_LINE: fixed Phase: SELECT|ACT|CONTINUE prefix regex")
});

/// Single owner of the author-body cleanup invariants. Used by:
/// - the universal compositor when wrapping authored non-FSM functions, and
/// - the phase-executor compositor when inlining IR `prompt_template` text.
pub struct AuthorBodySanitizer;

impl AuthorBodySanitizer {
    /// Normalize an authored function's `prompt #"..."#` inner body so the compositor can place
    /// it in the middle of the canonical skeleton without duplication. Order matters: catalog
    /// stripping runs **first** so a prior canonical block (from a previous build) collapses
    /// before transcript / output_format strips touch it.
    pub fn for_authored(inner: &str) -> String {
        let s = strip_tool_schema_prelude_jinja_blocks(inner);
        let s = strip_canonical_archive_prefix_paragraph(&s);
        let s = strip_legacy_archive_guidance_paragraphs(&s);
        let s = strip_authored_selection_hint(&s);
        let s = strip_conversation_transcript_jinja_blocks(&s);
        let s = strip_standalone_directive_lines(&s);
        collapse_blank_runs(&s)
    }

    /// Stricter cleanup applied to IR `prompt_template` text inlined into generated phase
    /// executors. Strips author transcript / output_format directives plus legacy bracket /
    /// `Phase: SELECT|ACT|CONTINUE` cues that codegen now owns.
    pub fn for_phase_ir(template: &str) -> String {
        let s = strip_tool_schema_prelude_jinja_blocks(template);
        let s = strip_canonical_archive_prefix_paragraph(&s);
        let s = strip_legacy_archive_guidance_paragraphs(&s);
        let s = strip_authored_selection_hint(&s);
        let s = strip_standalone_directive_lines(&s);
        let s = strip_conversation_transcript_jinja_blocks(&s);
        let s = strip_legacy_bracket_phase_tag_lines(&s);
        let s = strip_legacy_phase_cue_lines(&s);
        collapse_blank_runs(&s)
    }
}

fn strip_standalone_directive_lines(template: &str) -> String {
    template
        .lines()
        .filter(|line| {
            let t = line.trim_end_matches('\r').trim();
            !is_standalone_output_format_line(t)
                && !is_standalone_conversation_transcript_line(t)
                && !is_standalone_role_directive_line(t)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_standalone_output_format_line(s: &str) -> bool {
    matches!(
        s,
        "{{ ctx.output_format }}"
            | "{{ctx.output_format}}"
            | "{ ctx.output_format }"
            | "{ctx.output_format}"
    )
}

fn is_standalone_conversation_transcript_line(s: &str) -> bool {
    matches!(
        s,
        "{{ ctx.tags['conversation_transcript'] }}"
            | "{{ctx.tags['conversation_transcript']}}"
            | "{{ ctx.tags.conversation_transcript }}"
            | "{{ctx.tags.conversation_transcript}}"
    )
}

fn is_standalone_role_directive_line(s: &str) -> bool {
    let compact = s.replace(' ', "");
    compact.starts_with("{_.role(") && compact.ends_with(")}")
}

fn strip_conversation_transcript_jinja_blocks(template: &str) -> String {
    let s = TRANSCRIPT_BLOCK_BRACKET
        .replace_all(template, "")
        .into_owned();
    TRANSCRIPT_BLOCK_DOT.replace_all(&s, "").into_owned()
}

fn strip_tool_schema_prelude_jinja_blocks(template: &str) -> String {
    let s = PRELUDE_BLOCK_BRACKET.replace_all(template, "").into_owned();
    PRELUDE_BLOCK_DOT.replace_all(&s, "").into_owned()
}

fn strip_legacy_bracket_phase_tag_lines(template: &str) -> String {
    template
        .lines()
        .filter(|line| {
            let t = line.trim();
            !t.starts_with("[OPEN]") && !t.starts_with("[ACT]") && !t.starts_with("[CONTINUE]")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_legacy_phase_cue_lines(template: &str) -> String {
    template
        .lines()
        .filter(|line| !LEGACY_PHASE_CUE_LINE.is_match(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Idempotency for repeated rewrites: drop the canonical archive prose paragraph if the body
/// literally starts with it (after leading whitespace). Only the exact canonical prefix is
/// stripped; authored archive prose stays intact.
fn strip_canonical_archive_prefix_paragraph(template: &str) -> String {
    let trimmed_start = template.trim_start_matches(['\r', '\n', ' ', '\t']);
    if let Some(rest) = trimmed_start.strip_prefix(SESSION_STEP_STABLE_PREFIX_BAML) {
        rest.to_string()
    } else {
        template.to_string()
    }
}

fn strip_legacy_archive_guidance_paragraphs(template: &str) -> String {
    template
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != LEGACY_ARCHIVE_HANDLE_GUIDANCE && trimmed != LEGACY_OPEN_FIELD_GUIDANCE
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_authored_selection_hint(template: &str) -> String {
    template
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != AUTHORED_SELECTION_HINT && !is_generated_selection_hint_line(trimmed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_generated_selection_hint_line(line: &str) -> bool {
    line.starts_with("Return exactly one `")
        || line == "Return exactly one JSON object."
        || line.starts_with("Select the object shape with discriminator `")
        || line == "Choose one object shape:"
        || (line.starts_with("If `") && line.contains("choose each item with discriminator `"))
        || line.starts_with("- `")
        || (line.starts_with("Set `")
            && line.contains("Do not mix fields from different object shapes."))
        || (line.starts_with("Set `") && line.contains("exactly for each `"))
        || line == "Do not mix fields from different object shapes."
        || line == "Do not add text before or after the JSON object."
}

fn collapse_blank_runs(template: &str) -> String {
    EXTRA_BLANK_RUNS.replace_all(template, "\n\n").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_authored_strips_standalone_output_format() {
        let s = AuthorBodySanitizer::for_authored("Body.\n\n{{ ctx.output_format }}\nMore.\n");
        assert!(!s.contains("{{ ctx.output_format }}"));
        assert!(s.contains("Body."));
        assert!(s.contains("More."));
    }

    #[test]
    fn for_authored_strips_transcript_jinja_block() {
        let s = AuthorBodySanitizer::for_authored(
            "Body.\n{% if ctx.tags['conversation_transcript'] %}\nPrior:\n{{ ctx.tags['conversation_transcript'] }}\n{% endif %}\n",
        );
        assert!(!s.contains("Prior:"));
        assert!(s.contains("Body."));
    }

    #[test]
    fn for_authored_strips_existing_catalog_block() {
        let s = AuthorBodySanitizer::for_authored(
            "{% if ctx.tags['tool_schema_prelude'] %}\nTool and session-step types (authoritative field shapes):\n{{ ctx.tags['tool_schema_prelude'] }}\n\n{% endif %}\nBody.\n",
        );
        assert!(!s.contains("tool_schema_prelude"));
        assert!(s.contains("Body."));
    }

    #[test]
    fn for_authored_strips_canonical_archive_prefix() {
        let with_prefix = format!("{}Body.\n", SESSION_STEP_STABLE_PREFIX_BAML);
        let s = AuthorBodySanitizer::for_authored(&with_prefix);
        assert!(!s.contains(SESSION_STEP_STABLE_PREFIX_BAML.trim()));
        assert!(s.contains("Body."));
    }

    #[test]
    fn for_authored_strips_legacy_archive_guidance() {
        let s = AuthorBodySanitizer::for_authored(&format!(
            "{LEGACY_ARCHIVE_HANDLE_GUIDANCE}\n\n{LEGACY_OPEN_FIELD_GUIDANCE}\n\nBody.\n"
        ));
        assert!(!s.contains(LEGACY_ARCHIVE_HANDLE_GUIDANCE));
        assert!(!s.contains(LEGACY_OPEN_FIELD_GUIDANCE));
        assert!(s.contains("Body."));
    }

    #[test]
    fn for_authored_is_idempotent() {
        let once = AuthorBodySanitizer::for_authored("Body.\n");
        let twice = AuthorBodySanitizer::for_authored(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn for_authored_strips_existing_selection_hint() {
        let s = AuthorBodySanitizer::for_authored(
            "Body.\n\nReturn exactly one output matching the schema below. Do not add extra text before or after it.\n\n",
        );
        assert!(!s.contains(AUTHORED_SELECTION_HINT));
        assert!(s.contains("Body."));
    }

    #[test]
    fn for_authored_strips_generated_tagged_union_hint_block() {
        let s = AuthorBodySanitizer::for_authored(
            "Body.\n\
             Return exactly one JSON object.\n\
             Select the object shape with discriminator `kind`:\n\
             - `kind: \"ready\"` -> Ready\n\
             - `kind: \"meta\"` -> Meta\n\
             Set `kind` exactly. Do not mix fields from different object shapes.\n\
             Do not add text before or after the JSON object.\n",
        );
        assert!(s.contains("Body."));
        assert!(!s.contains("Select the object shape with discriminator"));
        assert!(!s.contains("Do not add text before or after the JSON object."));
    }

    #[test]
    fn for_phase_ir_strips_legacy_cues_and_brackets() {
        let s = AuthorBodySanitizer::for_phase_ir(
            "Phase: SELECT — pick a tool\n[OPEN] do open\n[ACT] do act\nReal IR text.\n",
        );
        assert!(!s.contains("Phase: SELECT"));
        assert!(!s.contains("[OPEN]"));
        assert!(!s.contains("[ACT]"));
        assert!(s.contains("Real IR text."));
    }

    #[test]
    fn for_phase_ir_strips_standalone_output_format() {
        let s = AuthorBodySanitizer::for_phase_ir("Task.\n{{ ctx.output_format }}\nMore.\n");
        assert!(!s.contains("{{ ctx.output_format }}"));
        assert!(s.contains("Task."));
        assert!(s.contains("More."));
    }

    #[test]
    fn for_phase_ir_strips_single_brace_output_format_and_role_lines() {
        let s = AuthorBodySanitizer::for_phase_ir(
            "Task.\n{ ctx.output_format }\n{ _.role('user') }\n{ input }\n",
        );
        assert!(!s.contains("{ ctx.output_format }"));
        assert!(!s.contains("{ _.role('user') }"));
        assert!(s.contains("{ input }"));
    }

    #[test]
    fn for_phase_ir_strips_legacy_archive_guidance_and_generated_selection_lines() {
        let s = AuthorBodySanitizer::for_phase_ir(&format!(
            "{LEGACY_ARCHIVE_HANDLE_GUIDANCE}\n\n{LEGACY_OPEN_FIELD_GUIDANCE}\n\nTask.\nReturn exactly one JSON object.\nSelect the object shape with discriminator `kind`:\n- `kind: \"ready\"` -> Ready\nSet `kind` exactly. Do not mix fields from different object shapes.\nDo not add text before or after the JSON object.\n"
        ));
        assert!(!s.contains(LEGACY_ARCHIVE_HANDLE_GUIDANCE));
        assert!(!s.contains(LEGACY_OPEN_FIELD_GUIDANCE));
        assert!(!s.contains("Select the object shape with discriminator"));
        assert!(s.contains("Task."));
    }
}
