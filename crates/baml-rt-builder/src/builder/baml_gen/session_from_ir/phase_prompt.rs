// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Generated phase executor (`__entry` / `__active__*`, plus unified-primary roots) prompt
//! assembly. Delegates the canonical-order knowledge to
//! [`crate::builder::baml_gen::PromptCompositor`] — this module only owns:
//!
//! - The IR-template stripper alias to [`AuthorBodySanitizer::for_phase_ir`].
//! - The `client … prompt #""#` BAML wrapper that frames the composed prompt body.
//! - The compact, state-indexed phase-policy strings rendered after the post-history contract.

use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::{
    baml_gen::{
        AuthorBodySanitizer, PromptCompositor, ToolSessionPhaseSpec, UnifiedPrimaryPhaseSpec,
    },
    selection_hint::{
        render_step_executor_selection_hint_for_named_union,
        render_type_reference_contract_for_named_union,
    },
};

/// Which FSM hop this executor represents — entry, active session, or unified structured hop.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum PhaseHop {
    Entry,
    Active,
}

/// Test-only alias to [`AuthorBodySanitizer::for_phase_ir`] preserved so the existing fixture
/// tests (and historical doc cross-references) keep their wording. Production code calls
/// [`AuthorBodySanitizer::for_phase_ir`] directly.
#[cfg(test)]
fn strip_phase_executor_ir_template(template: &str) -> String {
    AuthorBodySanitizer::for_phase_ir(template)
}

/// Appended after the post-history output contract on unified-primary generated functions.
///
/// No ASCII double quotes inside: concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_UNIFIED_PRIMARY: &str = r#"

Phase policy:
- Derived state rule: tool-session ops are illegal unless they appear in the schema above.
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
    /// Compiled IR used to derive a centralized selection hint from `legal_type_names`.
    pub ir_signature: &'a IRSignature,
    /// Variant names admissible for this hop. Must match the generated function return union.
    pub legal_type_names: &'a [String],
    /// Compact state-indexed phase policy appended after the schema binding.
    pub phase_policy: &'static str,
}

impl ToolSessionPhasePromptSpec<'_> {
    /// Render the full BAML `client … prompt #"..."#` block via [`PromptCompositor::tool_session_phase`].
    pub(crate) fn emit_baml_prompt_body(self, client_name: &str, prompt_template: &str) -> String {
        let stripped = AuthorBodySanitizer::for_phase_ir(prompt_template);
        let output_contract = render_type_reference_contract_for_named_union(self.legal_type_names);
        let selection_hint = render_step_executor_selection_hint_for_named_union(
            self.legal_type_names,
            self.ir_signature,
        );
        let inner = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
            stripped_ir_body: &stripped,
            output_contract: &output_contract,
            selection_hint: &selection_hint,
            phase_policy: self.phase_policy,
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
    phase: PhaseHop,
    legal_type_names: &[String],
    _supplement_after_cue: Option<&str>,
    ir_signature: &IRSignature,
) -> String {
    let stripped = AuthorBodySanitizer::for_phase_ir(prompt_template);
    let output_contract = render_type_reference_contract_for_named_union(legal_type_names);
    let selection_hint =
        render_step_executor_selection_hint_for_named_union(legal_type_names, ir_signature);
    let phase_policy = match phase {
        PhaseHop::Entry => {
            crate::builder::baml_gen::session_from_ir::entry_phase_executor_suffix(legal_type_names)
        }
        PhaseHop::Active => {
            "\nPhase policy:\n- Derived state rule: this active return union has no `Open` variant.\n"
        }
    };
    let inner = PromptCompositor::tool_session_phase(ToolSessionPhaseSpec {
        stripped_ir_body: &stripped,
        output_contract: &output_contract,
        selection_hint: &selection_hint,
        phase_policy,
    });
    wrap_client_baml_prompt_body(client_name, &inner)
}

/// Unified planner/synthesis step executor: stable prefix + STRUCTURED cue + footer + constraint suffix.
pub(crate) fn phase_executor_prompt_body_unified_primary(
    client_name: &str,
    prompt_template: &str,
    legal_type_names: &[String],
    ir_signature: &IRSignature,
) -> String {
    let stripped = AuthorBodySanitizer::for_phase_ir(prompt_template);
    let output_contract = render_type_reference_contract_for_named_union(legal_type_names);
    let selection_hint =
        render_step_executor_selection_hint_for_named_union(legal_type_names, ir_signature);
    let inner = PromptCompositor::unified_primary_phase(UnifiedPrimaryPhaseSpec {
        stripped_ir_body: &stripped,
        output_contract: &output_contract,
        selection_hint: &selection_hint,
        phase_policy: PHASE_STEP_EXECUTOR_SUFFIX_UNIFIED_PRIMARY,
    });
    wrap_client_baml_prompt_body(client_name, &inner)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use baml_runtime::BamlRuntime;
    use tempfile::TempDir;

    use super::*;

    fn test_ir_signature() -> IRSignature {
        let dir = TempDir::new().expect("tempdir");
        let baml_src = dir.path().join("baml_src");
        fs::create_dir_all(&baml_src).expect("mkdir baml_src");
        let source = r##"
class ArchiveSearchReadStep {
  op "SearchRead"
  input string
}

class ArchivePageReadStep {
  op "PageRead"
  input string
}

class XOpenStep {
  op "Open"
  tool_name string
}

class FooSendStep {
  op "Send"
  input string
}

class FooSearchReadStep {
  op "SearchRead"
  input string
}

class FooPageReadStep {
  op "PageRead"
  input string
}

class FooFinishStep {
  op "Finish"
}

class FooAbortStep {
  op "Abort"
}

class WorkflowPlan {
  steps string[]
}

class CoordinatorStructuredAskUser {
  action "AskUser"
  question string
}

function TestShape() -> WorkflowPlan {
  client DefaultClient
  prompt #"test"#
}

client DefaultClient {
  provider openai
  options {
    model "gpt-4o-mini"
    api_key env.OPENAI_API_KEY
  }
}
"##;
        let path = baml_src.join("shape_test.baml");
        fs::write(&path, source).expect("write baml");
        let runtime = BamlRuntime::from_directory(
            &baml_src,
            std::collections::HashMap::<String, String>::new(),
            internal_baml_core::feature_flags::FeatureFlags::default(),
        )
        .expect("runtime");
        IRSignature::new_from_ir(runtime.ir.as_ref()).expect("signature")
    }

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
    fn phase_executor_prompt_body_active_hop_includes_no_open_phase_policy() {
        let legal = vec![
            "FooSendStep".to_string(),
            "FooSearchReadStep".to_string(),
            "FooFinishStep".to_string(),
        ];
        let ir = test_ir_signature();
        let out = phase_executor_prompt_body(
            "TestClient",
            "Body.\n{{ ctx.output_format }}",
            PhaseHop::Active,
            &legal,
            None,
            &ir,
        );
        assert!(
            out.contains("no `Open` variant"),
            "expected active-hop phase policy: {out}"
        );
    }

    #[test]
    fn phase_executor_prompt_body_includes_archive_preamble_before_template() {
        let legal = vec!["ArchiveSearchReadStep".to_string(), "XOpenStep".to_string()];
        let ir = test_ir_signature();
        let out = phase_executor_prompt_body(
            "TestClient",
            "Only the IR template.\n{{ ctx.output_format }}",
            PhaseHop::Entry,
            &legal,
            None,
            &ir,
        );
        assert!(
            out.contains("Only the IR template."),
            "expected IR body in prompt: {out}"
        );
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 0,
            "tool-session phases must not render the expanded per-hop schema after history: {out}"
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
        let contract_pos = out
            .find("Return exactly one JSON object of type `ArchiveSearchReadStep | XOpenStep`.")
            .expect("contract binding");
        assert!(
            hist_pos < ir_pos,
            "session history must precede IR task body: {out}"
        );
        assert!(
            ir_pos < contract_pos,
            "IR task body must precede the per-hop contract: {out}"
        );
        assert!(
            out.contains("Use `op` as the discriminator: \"Open\" | \"SearchRead\"."),
            "expected compact op-selection hint after contract binding: {out}"
        );
        assert!(
            out.contains("Archive refs: `@N`"),
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
        let ir = test_ir_signature();
        let template = r#"Do work.
{% if ctx.tags['conversation_transcript'] %}
Old label:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
{{ ctx.output_format }}"#;
        let out =
            phase_executor_prompt_body("TestClient", template, PhaseHop::Entry, &legal, None, &ir);
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
    fn tool_session_active_emit_includes_phase_policy_and_schema() {
        let legal = vec![
            "FooSendStep".to_string(),
            "FooSearchReadStep".to_string(),
            "FooPageReadStep".to_string(),
            "FooFinishStep".to_string(),
            "FooAbortStep".to_string(),
        ];
        let ir = test_ir_signature();
        const ACTIVE_POLICY: &str = "\nPhase policy:\n- Derived state rule: this active return union has no `Open` variant.\n";
        let spec = ToolSessionPhasePromptSpec {
            legal_type_names: &legal,
            ir_signature: &ir,
            phase_policy: ACTIVE_POLICY,
        };
        let out = spec.emit_baml_prompt_body("TestClient", "Body.\n{{ ctx.output_format }}");
        assert!(out.contains("Body."), "template: {out}");
        assert!(out.contains("Phase policy:"), "phase policy: {out}");
        assert!(!out.contains("[ACT]") && !out.contains("[CONTINUE]"));
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 0,
            "tool-session active must not render the expanded per-hop output_format: {out}"
        );
        assert!(
            out.contains("Return exactly one JSON object of type `FooAbortStep | FooFinishStep | FooPageReadStep | FooSearchReadStep | FooSendStep`."),
            "expected compact type-reference contract: {out}"
        );
        assert!(
            out.contains("Use `op` as the discriminator: \"Abort\" | \"Finish\" | \"PageRead\" | \"SearchRead\" | \"Send\"."),
            "expected compact op-selection hint: {out}"
        );
    }

    #[test]
    fn unified_primary_prompt_includes_schema_and_phase_policy() {
        let legal = vec![
            "ArchiveSearchReadStep".to_string(),
            "ArchivePageReadStep".to_string(),
            "WorkflowPlan".to_string(),
            "CoordinatorStructuredAskUser".to_string(),
        ];
        let ir = test_ir_signature();
        let out = phase_executor_prompt_body_unified_primary(
            "TestClient",
            "Planner body.\n{{ ctx.output_format }}",
            &legal,
            &ir,
        );
        assert!(out.contains("Planner body."), "template: {out}");
        assert!(out.contains("Phase policy:"), "{out}");
        assert!(
            out.matches("{{ ctx.output_format }}").count() == 0,
            "expected compact contract instead of output_format: {out}"
        );
        let hist_pos = out.find("Session history:").expect("session history");
        let task_pos = out.find("Planner body.").expect("task body");
        let contract_pos = out
            .find(
                "Return exactly one JSON object of type `ArchivePageReadStep | ArchiveSearchReadStep | CoordinatorStructuredAskUser | WorkflowPlan`.",
            )
            .expect("contract");
        assert!(
            hist_pos < task_pos,
            "session history must precede the task body on unified-primary hops: {out}"
        );
        assert!(
            task_pos < contract_pos,
            "task body must precede the output contract on unified-primary hops: {out}"
        );
    }
}
