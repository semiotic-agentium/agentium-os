//! Single owner of canonical model-facing prompt order.
//!
//! Callers describe the **shape** they want — authored non-FSM, unified-primary phase, or
//! tool-session phase (entry / active) — and the compositor emits a `String` whose opening is
//! byte-identical across every shape so OpenAI prefix-cache hits across all prompts in an
//! agent package. No caller pushes prefix / transcript / output-format strings by hand.
//!
//! Only one prompt in the workspace bypasses this skeleton: the synthetic catalog function
//! `AgentToolSchemaCatalog__bamlrt`, whose body is exactly `{{ ctx.output_format }}` so BAML's
//! renderer can produce the catalog text without recursing on itself.

use baml_rt_tools::{
    CONVERSATION_TRANSCRIPT_TAG, SESSION_STEP_STABLE_PREFIX_BAML, TOOL_SCHEMA_PRELUDE_TAG,
};

use super::prompt_normalize::AuthorBodySanitizer;

/// Spec for tool-session phase executor (`__entry` / `__active__*`) prompts.
pub struct ToolSessionPhaseSpec<'a> {
    /// Phase cue line (`Phase: ENTRY — ...`, `Phase: ACTIVE — for X: ...`).
    pub phase_cue: &'a str,
    /// Optional supplement appended right after the cue (e.g. tool-specific guidance).
    pub supplement_after_cue: Option<&'a str>,
    /// IR `prompt_template` text **after** [`AuthorBodySanitizer::for_phase_ir`] has cleaned it.
    pub stripped_ir_body: &'a str,
    /// Variant names admissible for this hop. Their JSON shapes live in the catalog at the top.
    pub legal_type_names: &'a [String],
    /// Generated selection hint derived from the narrowed return shape.
    pub selection_hint: &'a str,
    /// Phase FSM constraint suffix (entry / active prose).
    pub constraint_suffix: &'static str,
}

/// Spec for unified-primary phase executor prompts (`__entry` for unified roots).
pub struct UnifiedPrimaryPhaseSpec<'a> {
    pub phase_cue: &'a str,
    pub stripped_ir_body: &'a str,
    pub legal_type_names: &'a [String],
    pub selection_hint: &'a str,
    /// Phase-constraint suffix appended after the narrowed-union footer.
    pub extra_suffix: &'static str,
}

/// Single domain object — all prompt assembly flows through this. Stateless; methods just emit
/// strings according to the canonical segment order.
pub struct PromptCompositor;

impl PromptCompositor {
    /// Return the byte-identical canonical opening (stable archive policy + catalog if-block)
    /// shared by every prompt the compositor emits.
    pub fn canonical_opening() -> String {
        let mut s = String::with_capacity(
            SESSION_STEP_STABLE_PREFIX_BAML
                .len()
                .saturating_add(TOOL_SCHEMA_PRELUDE_TAG.len().saturating_mul(4))
                .saturating_add(96),
        );
        push_stable_archive_prefix(&mut s);
        push_tool_schema_prelude_if_block(&mut s);
        s
    }

    /// Render an authored non-FSM function's `prompt #"..."#` inner body in canonical order:
    /// stable prefix → catalog if-block → sanitized author body → transcript if-block → output
    /// binding line. Used by the BAML source rewriter on every authored function except the
    /// synthetic catalog and the IR-inlined parents (session-plan + unified-primary).
    pub fn authored_non_fsm(author_inner: &str, selection_hint: &str) -> String {
        let sanitized = AuthorBodySanitizer::for_authored(author_inner);
        let trimmed = sanitized.trim();

        let mut out = String::with_capacity(author_inner.len() + 512);
        push_stable_archive_prefix(&mut out);
        push_tool_schema_prelude_if_block(&mut out);
        push_task_body(&mut out, trimmed);
        push_transcript_if_block(&mut out);
        push_selection_hint(&mut out, selection_hint);
        push_canonical_output_format_line(&mut out);
        out
    }

    /// Render a tool-session phase executor (`__entry` / `__active__*`) prompt body. Same
    /// opening as authored non-FSM, then phase cue + supplement + IR body + transcript + the
    /// narrowed-union footer (no per-hop `{{ ctx.output_format }}` — schemas come from the
    /// catalog tag at the top).
    pub fn tool_session_phase(spec: ToolSessionPhaseSpec<'_>) -> String {
        let mut out = String::with_capacity(spec.stripped_ir_body.len() + 1024);
        push_stable_archive_prefix(&mut out);
        push_tool_schema_prelude_if_block(&mut out);
        push_phase_cue(&mut out, spec.phase_cue);
        push_supplement(&mut out, spec.supplement_after_cue);
        push_task_body(&mut out, spec.stripped_ir_body.trim());
        push_transcript_if_block(&mut out);
        push_narrowed_union_footer(&mut out, spec.legal_type_names, spec.selection_hint);
        out.push_str(spec.constraint_suffix);
        out
    }

    /// Render a unified-primary phase executor prompt body. Like `tool_session_phase` but with
    /// an output-format binding before the narrowed-union footer (unified-primary roots admit
    /// arbitrary structured outputs whose schema must be enumerated by BAML at render time).
    pub fn unified_primary_phase(spec: UnifiedPrimaryPhaseSpec<'_>) -> String {
        let mut out = String::with_capacity(spec.stripped_ir_body.len() + 1024);
        push_stable_archive_prefix(&mut out);
        push_tool_schema_prelude_if_block(&mut out);
        push_phase_cue(&mut out, spec.phase_cue);
        push_task_body(&mut out, spec.stripped_ir_body.trim());
        push_transcript_if_block(&mut out);
        push_canonical_output_format_line(&mut out);
        push_narrowed_union_footer(&mut out, spec.legal_type_names, spec.selection_hint);
        out.push_str(spec.extra_suffix);
        out
    }
}

fn push_stable_archive_prefix(out: &mut String) {
    out.push_str(SESSION_STEP_STABLE_PREFIX_BAML);
}

fn push_tool_schema_prelude_if_block(out: &mut String) {
    out.push_str("{% if ctx.tags['");
    out.push_str(TOOL_SCHEMA_PRELUDE_TAG);
    out.push_str("'] %}\nTool and session-step types (authoritative field shapes):\n{{ ctx.tags['");
    out.push_str(TOOL_SCHEMA_PRELUDE_TAG);
    out.push_str("'] }}\n\n{% endif %}");
}

fn push_phase_cue(out: &mut String, cue: &str) {
    out.push_str(cue);
}

fn push_supplement(out: &mut String, supplement: Option<&str>) {
    if let Some(s) = supplement
        && !s.is_empty()
    {
        out.push_str(s);
        if !s.ends_with('\n') {
            out.push('\n');
        }
    }
}

fn push_task_body(out: &mut String, trimmed_body: &str) {
    if !trimmed_body.is_empty() {
        out.push_str(trimmed_body);
        if !trimmed_body.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
}

fn push_transcript_if_block(out: &mut String) {
    out.push_str("{% if ctx.tags['");
    out.push_str(CONVERSATION_TRANSCRIPT_TAG);
    out.push_str("'] %}\nSession history:\n{{ ctx.tags['");
    out.push_str(CONVERSATION_TRANSCRIPT_TAG);
    out.push_str("'] }}\n{% endif %}\n");
}

fn push_canonical_output_format_line(out: &mut String) {
    out.push('\n');
    out.push_str("{{ ctx.output_format }}\n");
}

fn push_selection_hint(out: &mut String, selection_hint: &str) {
    if selection_hint.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(selection_hint.trim_end());
    out.push('\n');
}

fn push_narrowed_union_footer(out: &mut String, legal_type_names: &[String], selection_hint: &str) {
    out.push_str("\n---\n");
    out.push_str("Narrowed return union for this hop only:\n");
    for name in legal_type_names {
        out.push_str("- ");
        out.push_str(name);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(selection_hint.trim_end());
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_opening_is_byte_identical_across_shapes() {
        let opening = PromptCompositor::canonical_opening();
        let authored = PromptCompositor::authored_non_fsm(
            "Body.",
            "Return exactly one output matching the schema below.\n",
        );
        let tool_session = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
            phase_cue: "Phase: ENTRY\n",
            supplement_after_cue: None,
            stripped_ir_body: "IR body.",
            legal_type_names: &["StepType".to_string()],
            selection_hint: "Return exactly one JSON object.\n",
            constraint_suffix: "",
        });
        let unified = PromptCompositor::unified_primary_phase(UnifiedPrimaryPhaseSpec {
            phase_cue: "Phase: STRUCTURED\n",
            stripped_ir_body: "IR body.",
            legal_type_names: &["UnifiedOut".to_string()],
            selection_hint: "Return exactly one JSON object.\n",
            extra_suffix: "",
        });
        assert!(authored.starts_with(&opening));
        assert!(tool_session.starts_with(&opening));
        assert!(unified.starts_with(&opening));
    }

    #[test]
    fn authored_non_fsm_emits_one_output_format_binding() {
        let s = PromptCompositor::authored_non_fsm(
            "Plan things.",
            "Return exactly one output matching the schema below.\n",
        );
        assert_eq!(s.matches("{{ ctx.output_format }}").count(), 1);
        assert!(s.contains("Plan things."));
    }

    #[test]
    fn tool_session_phase_omits_per_hop_output_format() {
        let s = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
            phase_cue: "Phase: ENTRY\n",
            supplement_after_cue: None,
            stripped_ir_body: "IR body.",
            legal_type_names: &["A".to_string(), "B".to_string()],
            selection_hint: "Return exactly one JSON object.\n",
            constraint_suffix: "",
        });
        assert!(!s.contains("{{ ctx.output_format }}"));
        assert!(s.contains("- A\n"));
        assert!(s.contains("- B\n"));
        assert!(s.contains("Return exactly one JSON object."));
    }

    #[test]
    fn unified_primary_includes_output_format_and_footer() {
        let s = PromptCompositor::unified_primary_phase(UnifiedPrimaryPhaseSpec {
            phase_cue: "Phase: STRUCTURED\n",
            stripped_ir_body: "IR body.",
            legal_type_names: &["A".to_string()],
            selection_hint: "Return exactly one JSON object.\n",
            extra_suffix: "",
        });
        assert_eq!(s.matches("{{ ctx.output_format }}").count(), 1);
        assert!(s.contains("- A\n"));
        let of_pos = s.find("{{ ctx.output_format }}").expect("of present");
        let footer_pos = s.find("Narrowed return union").expect("footer present");
        assert!(
            of_pos < footer_pos,
            "output_format must precede narrowed union footer"
        );
    }
}
