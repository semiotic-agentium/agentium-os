//! Phase executor prompt algebra: stable-prefix concatenation, strip/re-append `output_format`,
//! phase cue, and narrowed-union footer. The parent module holds IR walking and polymorphic class
//! emission.

use baml_rt_tools::SESSION_STEP_STABLE_PREFIX_BAML;

/// Which FSM hop this executor represents — select, first post-open act, continue, or unified
/// structured hop (plan / synthesis / archive reads / AskUser without a host tool Open).
/// Tool-phase variants are exercised in unit tests; emitted session-plan prompts still use the
/// legacy composer in `session_from_ir/mod.rs` until fully migrated onto [`compose_phase_prompt_core`].
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum PhaseHop<'a> {
    Select,
    Act {
        tool_display_name: &'a str,
    },
    Continue {
        tool_display_name: &'a str,
    },
    /// Unified primary hop: same prefix/footer algebra as tool phases; union lists reads +
    /// structured outputs + optional AskUser (see generated planner/synthesis roots).
    UnifiedPrimary,
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
        PhaseHop::UnifiedPrimary => {
            "Phase: STRUCTURED — emit exactly one JSON root from the narrowed union below; no ad-hoc prose outside the schema. \
Prefer archive reads (SearchRead / PageRead) only when you must ground on @N from conversation history. \
If you must ask the operator one clarifying question before emitting your plan or final structured reply, \
emit the AskUser-shaped variant from the union (field action='AskUser'), not free text—the executor will run another hop with updated history. \
Otherwise emit your WorkflowPlan, CoordinatorAnswer, or other structured output type named in the footer.\n\n"
                .to_string()
        }
    }
}

/// Appended after `{{ ctx.output_format }}` on unified-primary generated functions.
///
/// No ASCII double quotes inside: concatenated into BAML `prompt #""#` literals.
pub(crate) const PHASE_STEP_EXECUTOR_SUFFIX_UNIFIED_PRIMARY: &str = r#"

PHASE CONSTRAINT (structured unified hop): The JSON root must match ONLY one variant in the narrowed union above—archive SearchRead/PageRead steps (op exactly SearchRead or PageRead; use ArchiveSearchReadInput / ArchivePageReadInput; archive_ref uses @N never #N), your structured plan or answer type, or the AskUser object with action exactly AskUser and a concise question. Do not emit Open/Send/Finish tool-session steps here unless that variant is explicitly listed in the union. Never use op Read.
"#;

pub(crate) fn append_phase_footer(body: &mut String, legal_type_names: &[String]) {
    body.push_str("\n---\n");
    body.push_str("Narrowed return union for this hop only:\n");
    for name in legal_type_names {
        body.push_str("- ");
        body.push_str(name);
        body.push('\n');
    }
}

/// Wrap IR/template fragments in `client … prompt #""#` without applying `format!` to `prompt_inner`.
pub(crate) fn wrap_client_baml_prompt_body(client_name: &str, prompt_inner: &str) -> String {
    let mut s = String::with_capacity(prompt_inner.len() + client_name.len() + 24);
    s.push_str("\n  client ");
    s.push_str(client_name);
    s.push_str("\n  prompt #\"");
    s.push_str(prompt_inner);
    s.push_str("\"#\n");
    s
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
#[allow(dead_code)] // Used by unit tests; IR emission uses `phase_executor_prompt_body_unified_primary` + `session_from_ir` legacy helper.
pub(crate) fn phase_executor_prompt_body(
    client_name: &str,
    prompt_template: &str,
    phase: PhaseHop<'_>,
    legal_type_names: &[String],
) -> String {
    let core = compose_phase_prompt_core(prompt_template, phase, legal_type_names);
    let mut inner = String::new();
    inner.push_str(SESSION_STEP_STABLE_PREFIX_BAML);
    inner.push_str(&core);
    wrap_client_baml_prompt_body(client_name, &inner)
}

/// Unified planner/synthesis step executor: stable prefix + STRUCTURED cue + footer + constraint suffix.
pub(crate) fn phase_executor_prompt_body_unified_primary(
    client_name: &str,
    prompt_template: &str,
    legal_type_names: &[String],
) -> String {
    let mut core =
        compose_phase_prompt_core(prompt_template, PhaseHop::UnifiedPrimary, legal_type_names);
    core.push_str(PHASE_STEP_EXECUTOR_SUFFIX_UNIFIED_PRIMARY);
    let mut inner = String::new();
    inner.push_str(SESSION_STEP_STABLE_PREFIX_BAML);
    inner.push_str(&core);
    wrap_client_baml_prompt_body(client_name, &inner)
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
            out.contains("Archive: a `tool: @N`"),
            "expected embedded archive policy prefix in phase prompt: {out}"
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

    #[test]
    fn unified_primary_prompt_includes_structured_cue_and_archive_types_in_footer() {
        let legal = vec![
            "ArchiveSearchReadStep".to_string(),
            "ArchivePageReadStep".to_string(),
            "WorkflowPlan".to_string(),
            "CoordinatorStructuredAskUser".to_string(),
        ];
        let out = phase_executor_prompt_body_unified_primary(
            "TestClient",
            "Planner body.\n{{ ctx.output_format }}",
            &legal,
        );
        assert!(out.contains("Phase: STRUCTURED"), "cue: {out}");
        assert!(out.contains("Planner body."), "template: {out}");
        assert!(out.contains("- WorkflowPlan"), "footer: {out}");
        assert!(
            out.contains("PHASE CONSTRAINT (structured unified hop)"),
            "{out}"
        );
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 1,
            "expected single output_format: {out}"
        );
    }
}
