//! Phase executor prompt algebra: stable-prefix concatenation, strip/re-append `output_format`,
//! phase cue, and narrowed-union footer. The parent module holds IR walking and polymorphic class
//! emission.

use baml_rt_tools::SESSION_STEP_STABLE_PREFIX_BAML;

/// Which FSM hop this executor represents — select, first post-open act, or continue.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PhaseHop<'a> {
    Select,
    Act { tool_display_name: &'a str },
    Continue { tool_display_name: &'a str },
}

/// Remove lines that are only `{{ ctx.output_format }}` (optional whitespace / CRLF).
pub(crate) fn strip_standalone_output_format_directives(template: &str) -> String {
    template
        .lines()
        .filter(|line| !is_standalone_output_format_line(line.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_standalone_output_format_line(trimmed: &str) -> bool {
    let s = trimmed.trim_end_matches('\r').trim();
    s == "{{ ctx.output_format }}" || s == "{{ctx.output_format}}"
}

pub(crate) fn phase_cue_line(phase: PhaseHop<'_>) -> String {
    match phase {
        PhaseHop::Select => {
            "Phase: SELECT — read a visible archive (@N) or Open a tool session; legal ops only as in the narrowed union below.\n\n"
                .to_string()
        }
        PhaseHop::Act { tool_display_name: t } => {
            format!(
                "Phase: ACT — first post-Open hop for {t}: SearchRead, PageRead, or Send; legal ops only as below.\n\n"
            )
        }
        PhaseHop::Continue { tool_display_name: t } => {
            format!(
                "Phase: CONTINUE — for {t}: SearchRead, PageRead, re-Send, or Finish; legal ops only as below.\n\n"
            )
        }
    }
}

pub(crate) fn append_phase_footer(body: &mut String, legal_type_names: &[String]) {
    body.push_str("\n---\n");
    body.push_str("Narrowed return union for this hop only:\n");
    for name in legal_type_names {
        body.push_str("- ");
        body.push_str(name);
        body.push('\n');
    }
}

pub(crate) fn compose_phase_prompt_core(
    prompt_template: &str,
    phase: PhaseHop<'_>,
    legal_type_names: &[String],
) -> String {
    let stripped = strip_standalone_output_format_directives(prompt_template);
    let mut out = phase_cue_line(phase);
    let trimmed_stripped = stripped.trim();
    if !trimmed_stripped.is_empty() {
        out.push_str(trimmed_stripped);
        if !trimmed_stripped.ends_with('\n') {
            out.push('\n');
        }
    }
    append_phase_footer(&mut out, legal_type_names);
    out.push_str("\n{{ ctx.output_format }}\n");
    out
}

/// `client` + `prompt #""#` for a step executor. Uses concatenation so IR text is not passed
/// through `format!` — the stable-prefix Jinja, composed prompt core, and closing `"` must not use
/// `format!` on IR/template fragments.
pub(crate) fn phase_executor_prompt_body(
    client_name: &str,
    prompt_template: &str,
    phase: PhaseHop<'_>,
    legal_type_names: &[String],
) -> String {
    let core = compose_phase_prompt_core(prompt_template, phase, legal_type_names);
    let mut s = String::new();
    s.push_str(&format!("\n  client {client_name}\n  prompt #\""));
    s.push_str(SESSION_STEP_STABLE_PREFIX_BAML);
    s.push_str(&core);
    s.push_str("\"#\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_executor_prompt_body_includes_archive_preamble_before_template() {
        let legal = vec!["ArchiveSearchReadStep".to_string(), "XOpenStep".to_string()];
        let out = phase_executor_prompt_body(
            "TestClient",
            "Only the IR template.\n{{ ctx.output_format }}",
            PhaseHop::Select,
            &legal,
        );
        assert!(
            out.contains("Only the IR template."),
            "expected IR body after phase cue: {out}"
        );
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 1,
            "expected exactly one output_format line: {out}"
        );
        assert!(out.contains("Phase: SELECT"), "expected phase cue: {out}");
        assert!(
            out.contains("Narrowed return union for this hop only:"),
            "expected footer: {out}"
        );
        assert!(
            out.contains("- ArchiveSearchReadStep"),
            "expected footer bullet: {out}"
        );
        assert!(
            out.contains(baml_rt_tools::SESSION_STEP_STABLE_PREFIX_BAML.trim())
                || out.contains("session_step_stable_prefix"),
            "expected session_step_stable_prefix jinja: {out}"
        );
        assert!(
            !out.contains("[OPEN]") && !out.contains("[ACT]") && !out.contains("[CONTINUE]"),
            "expected no legacy phase tag preambles: {out}"
        );
    }

    #[test]
    fn strip_standalone_output_format_removes_only_standalone_lines() {
        let t = "Hello\n{{ ctx.output_format }}\nWorld\n";
        assert_eq!(strip_standalone_output_format_directives(t), "Hello\nWorld");
        let embedded = "Say {{ ctx.output_format }} here\n";
        assert_eq!(
            strip_standalone_output_format_directives(embedded),
            embedded.trim_end()
        );
    }
}
