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

#[derive(Clone, Copy)]
pub(crate) struct StablePrefixSpec;

pub(crate) enum OutputContractSpec<'a> {
    OutputFormat,
    Text(&'a str),
}

pub(crate) struct PostHistorySpec<'a> {
    pub task_body: &'a str,
    pub output_contract: OutputContractSpec<'a>,
    pub selection_hint: &'a str,
    pub phase_policy: Option<&'a str>,
}

pub(crate) struct PromptLayout<'a> {
    pub stable_prefix: StablePrefixSpec,
    pub post_history: PostHistorySpec<'a>,
}

/// Spec for tool-session phase executor (`__entry` / `__active__*`) prompts.
pub struct ToolSessionPhaseSpec<'a> {
    /// IR `prompt_template` text **after** [`AuthorBodySanitizer::for_phase_ir`] has cleaned it.
    pub stripped_ir_body: &'a str,
    /// Compact post-history contract that references the legal named return members directly.
    pub output_contract: &'a str,
    /// Generated discriminator / shape hint derived from the narrowed return shape.
    pub selection_hint: &'a str,
    /// Compact state-indexed phase policy rendered after the post-history contract.
    pub phase_policy: &'a str,
}

/// Spec for unified-primary phase executor prompts (`__entry` for unified roots).
pub struct UnifiedPrimaryPhaseSpec<'a> {
    pub stripped_ir_body: &'a str,
    pub output_contract: &'a str,
    pub selection_hint: &'a str,
    /// Compact state-indexed phase policy rendered after the post-history contract.
    pub phase_policy: &'a str,
}

/// Single domain object — all prompt assembly flows through this. Stateless; methods just emit
/// strings according to the canonical segment order.
pub struct PromptCompositor;

impl PromptCompositor {
    /// Return the byte-identical canonical opening (stable archive policy + IR-derived tool
    /// vocabulary + canonical transcript block) shared by every prompt the compositor emits.
    pub fn canonical_opening() -> String {
        let layout = PromptLayout {
            stable_prefix: StablePrefixSpec,
            post_history: PostHistorySpec {
                task_body: "",
                output_contract: OutputContractSpec::OutputFormat,
                selection_hint: "",
                phase_policy: None,
            },
        };
        let mut out = String::with_capacity(
            SESSION_STEP_STABLE_PREFIX_BAML
                .len()
                .saturating_add(TOOL_SCHEMA_PRELUDE_TAG.len().saturating_mul(4))
                .saturating_add(CONVERSATION_TRANSCRIPT_TAG.len().saturating_mul(4))
                .saturating_add(128),
        );
        push_stable_prefix(&mut out, layout.stable_prefix);
        push_transcript_if_block(&mut out);
        out
    }

    /// Render an authored non-FSM function's `prompt #"..."#` inner body in canonical order:
    /// stable prefix → transcript if-block → sanitized author body → output binding line →
    /// generated selection hint. Used by the BAML source rewriter on every authored function
    /// except the IR-inlined parents (session-plan + unified-primary).
    pub fn authored_non_fsm(author_inner: &str, selection_hint: &str) -> String {
        let sanitized = AuthorBodySanitizer::for_authored(author_inner);
        let trimmed = sanitized.trim();
        Self::render_layout(PromptLayout {
            stable_prefix: StablePrefixSpec,
            post_history: PostHistorySpec {
                task_body: trimmed,
                output_contract: OutputContractSpec::OutputFormat,
                selection_hint,
                phase_policy: None,
            },
        })
    }

    /// Render a tool-session phase executor (`__entry` / `__active__*`) prompt body. Same
    /// opening as authored non-FSM, then the IR task body, a compact type-reference contract,
    /// the generated selection hint, and a compact state-indexed phase policy.
    pub fn tool_session_phase(spec: ToolSessionPhaseSpec<'_>) -> String {
        Self::render_layout(PromptLayout {
            stable_prefix: StablePrefixSpec,
            post_history: PostHistorySpec {
                task_body: spec.stripped_ir_body.trim(),
                output_contract: OutputContractSpec::Text(spec.output_contract),
                selection_hint: spec.selection_hint,
                phase_policy: Some(spec.phase_policy),
            },
        })
    }

    /// Render a unified-primary phase executor prompt body. Same stable prefix and post-history
    /// schema binding order as tool phases.
    pub fn unified_primary_phase(spec: UnifiedPrimaryPhaseSpec<'_>) -> String {
        Self::render_layout(PromptLayout {
            stable_prefix: StablePrefixSpec,
            post_history: PostHistorySpec {
                task_body: spec.stripped_ir_body.trim(),
                output_contract: OutputContractSpec::Text(spec.output_contract),
                selection_hint: spec.selection_hint,
                phase_policy: Some(spec.phase_policy),
            },
        })
    }

    fn render_layout(layout: PromptLayout<'_>) -> String {
        let mut out = String::with_capacity(layout.post_history.task_body.len() + 768);
        push_stable_prefix(&mut out, layout.stable_prefix);
        push_transcript_if_block(&mut out);
        push_task_body(&mut out, layout.post_history.task_body);
        push_output_contract(&mut out, layout.post_history.output_contract);
        push_selection_hint(&mut out, layout.post_history.selection_hint);
        push_phase_policy(&mut out, layout.post_history.phase_policy);
        out
    }
}

fn push_stable_prefix(out: &mut String, _spec: StablePrefixSpec) {
    out.push_str(SESSION_STEP_STABLE_PREFIX_BAML);
    push_tool_schema_prelude_if_block(out);
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
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("{% if ctx.tags['");
    out.push_str(CONVERSATION_TRANSCRIPT_TAG);
    out.push_str("'] %}\nSession history:\n{{ ctx.tags['");
    out.push_str(CONVERSATION_TRANSCRIPT_TAG);
    out.push_str("'] }}\n{% endif %}\n");
}

fn push_output_contract(out: &mut String, contract: OutputContractSpec<'_>) {
    match contract {
        OutputContractSpec::OutputFormat => out.push_str("{{ ctx.output_format }}\n"),
        OutputContractSpec::Text(text) => {
            let trimmed = text.trim_end();
            if !trimmed.is_empty() {
                out.push_str(trimmed);
                out.push('\n');
            }
        }
    }
}

fn push_selection_hint(out: &mut String, selection_hint: &str) {
    if selection_hint.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(selection_hint.trim_end());
    out.push('\n');
}

fn push_phase_policy(out: &mut String, phase_policy: Option<&str>) {
    if let Some(policy) = phase_policy {
        let trimmed = policy.trim();
        if !trimmed.is_empty() {
            out.push('\n');
            out.push_str(trimmed);
            out.push('\n');
        }
    }
}

fn push_tool_schema_prelude_if_block(out: &mut String) {
    out.push_str("{% if ctx.tags['");
    out.push_str(TOOL_SCHEMA_PRELUDE_TAG);
    out.push_str("'] %}\nTool and operation types:\n{{ ctx.tags['");
    out.push_str(TOOL_SCHEMA_PRELUDE_TAG);
    out.push_str("'] }}\n\n{% endif %}\n");
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
            stripped_ir_body: "IR body.",
            output_contract: "Return exactly one JSON object of type `XOpenStep | ArchiveSearchReadStep`.\n",
            selection_hint: "Use `op` as the discriminator: \"Open\" | \"SearchRead\".\nDo not add text before or after the JSON object.\n",
            phase_policy: "Phase policy:\n- Entry excludes Send.\n",
        });
        let unified = PromptCompositor::unified_primary_phase(UnifiedPrimaryPhaseSpec {
            stripped_ir_body: "IR body.",
            output_contract: "Return exactly one JSON object of type `WorkflowPlan | CoordinatorStructuredAskUser`.\n",
            selection_hint: "Choose the matching object type.\nDo not add text before or after the JSON object.\n",
            phase_policy: "Phase policy:\n- Structured only.\n",
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
        let hist_pos = s.find("Session history:").expect("history");
        let task_pos = s.find("Plan things.").expect("task");
        assert!(hist_pos < task_pos, "history must precede task body");
    }

    #[test]
    fn tool_session_phase_includes_compact_contract_after_history() {
        let s = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
            stripped_ir_body: "IR body.",
            output_contract: "Return exactly one JSON object of type `XOpenStep | ArchiveSearchReadStep`.\n",
            selection_hint: "Use `op` as the discriminator: \"Open\" | \"SearchRead\".\nDo not add text before or after the JSON object.\n",
            phase_policy: "Phase policy:\n- Entry excludes Send.\n",
        });
        assert_eq!(s.matches("{{ ctx.output_format }}").count(), 0);
        assert!(s.contains(
            "Return exactly one JSON object of type `XOpenStep | ArchiveSearchReadStep`."
        ));
        assert!(s.contains("Phase policy:"));
        let hist_pos = s.find("Session history:").expect("history");
        let task_pos = s.find("IR body.").expect("task");
        let contract_pos = s
            .find("Return exactly one JSON object of type `XOpenStep | ArchiveSearchReadStep`.")
            .expect("contract");
        assert!(hist_pos < task_pos, "history must precede task body");
        assert!(
            task_pos < contract_pos,
            "task body must precede output contract"
        );
    }

    #[test]
    fn unified_primary_includes_compact_contract_after_history() {
        let s = PromptCompositor::unified_primary_phase(UnifiedPrimaryPhaseSpec {
            stripped_ir_body: "IR body.",
            output_contract: "Return exactly one JSON object of type `WorkflowPlan | CoordinatorStructuredAskUser`.\n",
            selection_hint: "Choose the matching object type.\nDo not add text before or after the JSON object.\n",
            phase_policy: "Phase policy:\n- Structured only.\n",
        });
        assert_eq!(s.matches("{{ ctx.output_format }}").count(), 0);
        assert!(s.contains("Phase policy:"));
        let hist_pos = s.find("Session history:").expect("history");
        let task_pos = s.find("IR body.").expect("task");
        let contract_pos = s
            .find("Return exactly one JSON object of type `WorkflowPlan | CoordinatorStructuredAskUser`.")
            .expect("contract present");
        assert!(hist_pos < task_pos, "history must precede task body");
        assert!(
            task_pos < contract_pos,
            "task body must precede output contract"
        );
    }
}
