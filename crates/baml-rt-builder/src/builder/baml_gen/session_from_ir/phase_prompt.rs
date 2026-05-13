//! Generated phase executor (`__entry` / `__active__*`, plus unified-primary roots) prompt
//! assembly. Delegates the canonical-order knowledge to
//! [`crate::builder::baml_gen::PromptCompositor`] — this module only owns:
//!
//! - The [`PhaseHop`] cue selector and the cue text (`Phase: ENTRY` / `ACTIVE` / `STRUCTURED`).
//! - The IR-template stripper alias to [`AuthorBodySanitizer::for_phase_ir`].
//! - The `client … prompt #""#` BAML wrapper that frames the composed prompt body.
//! - The phase-FSM constraint suffix constants (entry / active / unified-primary).

use crate::builder::baml_gen::{
    AuthorBodySanitizer, PromptCompositor, ToolSessionPhaseSpec, UnifiedPrimaryPhaseSpec,
};

/// Which FSM hop this executor represents — entry, active session, or unified structured hop.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PhaseHop<'a> {
    Entry,
    Active {
        tool_display_name: &'a str,
    },
    /// Unified primary hop: same prefix/footer algebra as tool phases. The emitted prompt cue is
    /// **`Phase: STRUCTURED`** ([`phase_cue_line`]); this variant name is historical — treat it as
    /// "structured union hop" in codegen and docs.
    UnifiedPrimary,
}

/// Test-only alias to [`AuthorBodySanitizer::for_phase_ir`] preserved so the existing fixture
/// tests (and historical doc cross-references) keep their wording. Production code calls
/// [`AuthorBodySanitizer::for_phase_ir`] directly.
#[cfg(test)]
fn strip_phase_executor_ir_template(template: &str) -> String {
    AuthorBodySanitizer::for_phase_ir(template)
}

fn phase_cue_line(phase: PhaseHop<'_>) -> String {
    match phase {
        PhaseHop::Entry => {
            "Phase: ENTRY — reuse a visible archive (@N), ReadOnlyFinish without Open, or Open a tool session; legal ops only as in the narrowed union below.\n\n"
                .to_string()
        }
        PhaseHop::Active {
            tool_display_name: t,
        } => format!(
            "Phase: ACTIVE — for {t}: Send, SearchRead, PageRead, Finish (after Done Send only), or Abort; legal ops only as below.\n\n"
        ),
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
const PHASE_STEP_EXECUTOR_SUFFIX_UNIFIED_PRIMARY: &str = r#"

PHASE CONSTRAINT (structured unified hop): The JSON root must match ONLY one variant in the narrowed union above—archive SearchRead/PageRead steps (op exactly SearchRead or PageRead; use ArchiveSearchReadInput / ArchivePageReadInput; archive_ref uses @N never #N), your structured plan or answer type, or the AskUser object with action exactly AskUser and a concise question. Do not emit Open/Send/Finish tool-session steps here unless that variant is explicitly listed in the union. Never use op Read.
"#;

/// Wrap IR/template fragments in `client … prompt #""#` without applying `format!` to `prompt_inner`.
fn wrap_client_baml_prompt_body(client_name: &str, prompt_inner: &str) -> String {
    let mut s = String::with_capacity(prompt_inner.len() + client_name.len() + 24);
    s.push_str("\n  client ");
    s.push_str(client_name);
    s.push_str("\n  prompt #\"");
    s.push_str(prompt_inner);
    s.push_str("\"#\n");
    s
}

/// Typed inputs for one emitted tool-session phase executor (`__entry` / `__active__*`).
pub(crate) struct ToolSessionPhasePromptSpec<'a> {
    /// Which hop is being rendered; drives the emitted `Phase: …` cue and supplement tone.
    pub phase: PhaseHop<'a>,
    /// Variant names for the narrowed-union footer (must match the generated `function` return type).
    pub legal_type_names: &'a [String],
    /// Phase-specific FSM constraints (entry vs active suffixes from `mod.rs`) appended after
    /// the narrowed-union footer.
    pub constraint_suffix: &'static str,
    /// Optional prose immediately after the phase cue; empty or `None` is skipped.
    pub supplement_after_cue: Option<&'a str>,
}

impl ToolSessionPhasePromptSpec<'_> {
    /// Render the full BAML `client … prompt #"..."#` block via [`PromptCompositor::tool_session_phase`].
    pub(crate) fn emit_baml_prompt_body(self, client_name: &str, prompt_template: &str) -> String {
        let stripped = AuthorBodySanitizer::for_phase_ir(prompt_template);
        let cue = phase_cue_line(self.phase);
        let inner = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
            phase_cue: &cue,
            supplement_after_cue: self.supplement_after_cue,
            stripped_ir_body: &stripped,
            legal_type_names: self.legal_type_names,
            constraint_suffix: self.constraint_suffix,
        });
        wrap_client_baml_prompt_body(client_name, &inner)
    }
}

/// Test-only convenience that mirrors what `ToolSessionPhasePromptSpec::emit_baml_prompt_body`
/// would produce with no constraint suffix — used by the existing fixture tests in this file.
#[cfg(test)]
fn phase_executor_prompt_body(
    client_name: &str,
    prompt_template: &str,
    phase: PhaseHop<'_>,
    legal_type_names: &[String],
    supplement_after_cue: Option<&str>,
) -> String {
    let stripped = AuthorBodySanitizer::for_phase_ir(prompt_template);
    let cue = phase_cue_line(phase);
    let inner = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
        phase_cue: &cue,
        supplement_after_cue,
        stripped_ir_body: &stripped,
        legal_type_names,
        constraint_suffix: "",
    });
    wrap_client_baml_prompt_body(client_name, &inner)
}

/// Unified planner/synthesis step executor: stable prefix + STRUCTURED cue + footer + constraint suffix.
pub(crate) fn phase_executor_prompt_body_unified_primary(
    client_name: &str,
    prompt_template: &str,
    legal_type_names: &[String],
) -> String {
    let stripped = AuthorBodySanitizer::for_phase_ir(prompt_template);
    let cue = phase_cue_line(PhaseHop::UnifiedPrimary);
    let inner = PromptCompositor::unified_primary_phase(UnifiedPrimaryPhaseSpec {
        phase_cue: &cue,
        stripped_ir_body: &stripped,
        legal_type_names,
        extra_suffix: PHASE_STEP_EXECUTOR_SUFFIX_UNIFIED_PRIMARY,
    });
    wrap_client_baml_prompt_body(client_name, &inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_client_baml_prompt_body_frames_prompt_literal() {
        let out = wrap_client_baml_prompt_body("MyClient", "line1\nline2");
        assert!(out.starts_with("\n  client MyClient\n  prompt #\""));
        assert!(out.ends_with("\"#\n"));
        assert!(out.contains("line1\nline2"));
    }

    #[test]
    fn wrap_client_baml_prompt_body_empty_inner() {
        let out = wrap_client_baml_prompt_body("C", "");
        assert!(out.contains("prompt #\"\""));
    }

    #[test]
    fn wrap_client_baml_prompt_body_preserves_double_quotes_in_inner() {
        let inner = r#"say "hi""#;
        let out = wrap_client_baml_prompt_body("C", inner);
        assert!(out.contains(inner));
    }

    #[test]
    fn phase_executor_prompt_body_includes_archive_preamble_before_template() {
        let legal = vec!["ArchiveSearchReadStep".to_string(), "XOpenStep".to_string()];
        let out = phase_executor_prompt_body(
            "TestClient",
            "Only the IR template.\n{{ ctx.output_format }}",
            PhaseHop::Entry,
            &legal,
            None,
        );
        assert!(
            out.contains("Only the IR template."),
            "expected IR body after phase cue: {out}"
        );
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 0,
            "tool-session phases must not render per-hop BAML output_format — schemas live in the catalog: {out}"
        );
        assert!(
            out.contains("tool_schema_prelude"),
            "expected catalog Jinja tag block: {out}"
        );
        assert_eq!(
            out.matches("Session history:").count(),
            1,
            "single canonical session history injection: {out}"
        );
        let ir_pos = out
            .find("Only the IR template.")
            .expect("IR template fragment");
        let hist_pos = out.find("Session history:").expect("session history");
        let footer_pos = out
            .find("Narrowed return union for this hop only:")
            .expect("narrowed union footer");
        assert!(
            ir_pos < hist_pos,
            "IR task body must precede generated session history: {out}"
        );
        assert!(
            hist_pos < footer_pos,
            "narrowed union footer must follow session history (bottom of prompt): {out}"
        );
        assert!(out.contains("Phase: ENTRY"), "expected phase cue: {out}");
        assert!(
            out.contains("- ArchiveSearchReadStep"),
            "expected footer bullet: {out}"
        );
        assert!(
            out.contains("Emit exactly one JSON object matching one of the named types above"),
            "expected emit instruction at bottom: {out}"
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
    fn strip_phase_executor_ir_removes_bracket_transcript_block() {
        let t = r#"Task line.

{% if ctx.tags['conversation_transcript'] %}
Prior:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

More."#;
        let s = strip_phase_executor_ir_template(t);
        assert!(!s.contains("{% if ctx.tags['conversation_transcript'] %}"));
        assert!(s.contains("Task line."));
        assert!(s.contains("More."));
    }

    #[test]
    fn strip_phase_executor_ir_removes_legacy_bracket_lines_and_legacy_phase_cues() {
        let t = r#"[OPEN] Open a session with: foo/tool.

Task body line.

Phase: SELECT — duplicate cue from old docs.

  phase: act — lower spacing variant

Phase: CONTINUE trailing

Keep this."#;
        let s = strip_phase_executor_ir_template(t);
        assert!(!s.contains("[OPEN]"));
        assert!(!s.contains("Phase: SELECT"));
        assert!(!s.contains("phase: act"));
        assert!(!s.contains("Phase: CONTINUE"));
        assert!(s.contains("Task body line."));
        assert!(s.contains("Keep this."));
    }

    #[test]
    fn strip_phase_executor_ir_preserves_line_that_only_mentions_bracket_tag_mid_sentence() {
        let t = "Say [OPEN] is not a preamble when not at line start.\nNext.";
        let s = strip_phase_executor_ir_template(t);
        assert!(s.contains("[OPEN]"));
    }

    #[test]
    fn strip_phase_executor_ir_removes_dot_transcript_block() {
        let t = r#"Body
{% if ctx.tags.conversation_transcript %}
{{ ctx.tags.conversation_transcript }}
{% endif %}
Tail"#;
        let s = strip_phase_executor_ir_template(t);
        assert!(!s.contains("ctx.tags.conversation_transcript"));
        assert!(s.contains("Body"));
        assert!(s.contains("Tail"));
    }

    #[test]
    fn phase_executor_injects_session_history_once_even_if_author_had_block() {
        let legal = vec!["XOpenStep".to_string()];
        let template = r#"Do work.
{% if ctx.tags['conversation_transcript'] %}
Old label:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
{{ ctx.output_format }}"#;
        let out = phase_executor_prompt_body("TestClient", template, PhaseHop::Entry, &legal, None);
        assert_eq!(
            out.matches("Session history:").count(),
            1,
            "expected single generated history label: {out}"
        );
        assert!(
            !out.contains("Old label:"),
            "author transcript block must be stripped: {out}"
        );
        assert_eq!(
            out.matches("{% if ctx.tags['conversation_transcript'] %}")
                .count(),
            1,
            "exactly one generated transcript if-block: {out}"
        );
    }

    #[test]
    fn tool_session_active_emit_includes_supplement_constraint_and_no_legacy_phase_tag() {
        let legal = vec![
            "FooSendStep".to_string(),
            "FooSearchReadStep".to_string(),
            "FooPageReadStep".to_string(),
            "FooFinishStep".to_string(),
            "FooAbortStep".to_string(),
        ];
        const ACTIVE_SUFFIX: &str = "\n\nPHASE CONSTRAINT (active — test stub)";
        let supplement = "A foo/tool session is open. Emit Send.\n\n";
        let spec = ToolSessionPhasePromptSpec {
            phase: PhaseHop::Active {
                tool_display_name: "foo/tool",
            },
            legal_type_names: &legal,
            constraint_suffix: ACTIVE_SUFFIX,
            supplement_after_cue: Some(supplement),
        };
        let out = spec.emit_baml_prompt_body("TestClient", "Body.\n{{ ctx.output_format }}");
        assert!(out.contains("Phase: ACTIVE"), "cue: {out}");
        assert!(out.contains(supplement.trim_end()), "supplement: {out}");
        assert!(out.contains("Body."), "template: {out}");
        assert!(
            out.contains("PHASE CONSTRAINT (active — test stub)"),
            "suffix: {out}"
        );
        assert!(!out.contains("[ACT]") && !out.contains("[CONTINUE]"));
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 0,
            "tool-session active must not duplicate output_format: {out}"
        );
        assert!(
            out.contains("- FooSendStep"),
            "expected footer bullet: {out}"
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
        let hist_pos = out.find("Session history:").expect("session history");
        let of_pos = out.find("{{ ctx.output_format }}").expect("output_format");
        let footer_pos = out
            .find("Narrowed return union for this hop only:")
            .expect("narrowed union footer");
        assert!(
            hist_pos < of_pos,
            "session history must precede output_format on unified-primary hops: {out}"
        );
        assert!(
            of_pos < footer_pos,
            "narrowed union footer must follow output_format on unified-primary hops: {out}"
        );
    }
}
