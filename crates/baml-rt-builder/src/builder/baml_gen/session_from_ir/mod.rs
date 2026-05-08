//! Session plans and per-phase step executors generated from compiled BAML IR.
//!
//! Per-phase functions (`__select`, `__act__*`, `__continue__*`) share the parent session-plan
//! BAML function's `prompt_template`. Each hop uses [`phase_prompt::compose_phase_prompt_core`]:
//! [`SESSION_STEP_STABLE_PREFIX_BAML`](baml_rt_tools::SESSION_STEP_STABLE_PREFIX_BAML), a phase cue,
//! optional tool-specific supplement (tool list on select; discover_agents discipline on act/continue),
//! the stripped IR template, narrowed-union footer, `{{ ctx.output_format }}`, then a **phase-constraint
//! suffix** (legal JSON root shape for that hop). Without cue + suffix + footer the LLM often emits
//! malformed ops (`Read` vs `SearchRead`/`PageRead`, `#N` vs `@N`) that fail validation.
//!
//! **Suffix ↔ generated return types (tool phases):**
//! - **select** — [`select_phase_executor_suffix`] must agree with the `function __select -> …` union.
//! - **act** — [`PHASE_STEP_EXECUTOR_SUFFIX_ACT`] matches `Send | SearchRead | PageRead` only.
//! - **continue** — [`PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE`] adds `Finish` (generated `__continue__` omits `Abort`
//!   even when full `*SessionStep` unions in the prelude include it).

mod phase_prompt;

use std::{collections::HashMap, ops::Deref};

use baml_rt_tools::{SessionPlanTypeName, SessionTypeNames, tools::ToolFunctionMetadata};
use baml_types::ir_type::TypeGeneric;
use internal_baml_core::ir::ir_hasher::IRSignature;

use super::ir_type_print::{collect_union_type_names, type_ir_to_baml};
use crate::builder::error::{Result, write_line};

/// Appended to the shared session-coordination prompt on **select** hops so the model does not
/// emit a step wrapper when the IR return type is a bare Open step (parse failure).
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_SELECT: &str = r#"

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#;

/// Appended on **select** when the narrowed union is **only** `*OpenStep` variants (no Report / AskUser / etc.).
/// Using [`PHASE_STEP_EXECUTOR_SUFFIX_SELECT`] in that case invites `op: Report` and BAML parse failures.
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_SELECT_OPEN_ONLY: &str = r#"

PHASE CONSTRAINT (select — open only): The JSON root must be exactly one bare Open step: op must be Open; set tool_name and initial_input per the narrowed return type for this hop. Do NOT emit Report or AskUser. Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#;

/// Per-hop suffix for generated `__select` functions: branchy union vs Open-shaped union only.
///
/// Open-only when **every** member of `select_return` ends with `OpenStep` (matches [`SessionTypeNames::open_step`]).
/// Do **not** infer from `non_plan_types.is_empty()` alone — IR can list `FooOpenStep | BarSessionPlan` so
/// `non_plan_types` is non-empty while the legal select union is still open-only.
pub(crate) fn select_phase_executor_suffix(select_return: &[String]) -> &'static str {
    if select_return.is_empty() {
        return PHASE_STEP_EXECUTOR_SUFFIX_SELECT;
    }
    if select_return.iter().all(|t| t.ends_with("OpenStep")) {
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT_OPEN_ONLY
    } else {
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT
    }
}

/// Appended on **act** (first post-Open hop: Send or archive paging) — same archive discipline as continue.
const PHASE_STEP_EXECUTOR_SUFFIX_ACT: &str = r#"

PHASE CONSTRAINT (act — Send, Abort, or archive read): The JSON root must be exactly one Send, SearchRead, PageRead, or Abort step. op must be exactly the string Send, SearchRead, PageRead, or Abort — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#;

/// Appended on **continue** hops (Send | SearchRead | PageRead | Finish | Abort).
const PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE: &str = r#"

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, Finish, or Abort step. op must be Send, SearchRead, PageRead, Finish, or Abort — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#;

/// Injected into **act** and **continue** preambles for `system/discover_agents` only.
/// Models often add `required_capabilities` / subscription filters from inferred intent; those
/// filters are strict server-side and frequently yield zero rows, which bypasses query-only
/// fallback and looks like no agents exist.
const DISCOVER_AGENTS_SEND_DISCIPLINE: &str = r#"DISCOVERY INPUT RULE: For broad listing and routing, set only the `query` field (free-text match on name, package, description). Leave `required_capabilities`, `required_schema_versions`, and `required_source_kinds` null or omit them unless the user explicitly asked to filter by capability or event subscription. Do not add filters to narrow a vague intent — that often yields zero agents.

PAGING BEFORE RE-SEND: When a prior Send for this tool already archived an agents listing at @N, prefer SearchRead or PageRead on that @N (grep/limit/offset) to page or narrow — do not re-Send discover_agents with the same broad query as a substitute for pagination.

"#;

/// CONTINUE-only: act hop union has no Finish — keep Finish guidance out of [`DISCOVER_AGENTS_SEND_DISCIPLINE`].
const DISCOVER_AGENTS_CONTINUE_FINISH_HINT: &str = r#"FINISH DEFAULT ON CONTINUE: When history already shows a completed discover_agents archive (@N) and the listing is visible or confirmed empty (including zero agents), default to Finish — do not Send again with the same broad query unless deliberately refining filters per DISCOVERY INPUT RULE. Use SearchRead/PageRead only when line-level archive inspection or pagination is required.

"#;

/// Bundle emitted from IR: polymorphic Open/plan classes and per-phase executor functions.
#[derive(Debug, Default, Clone)]
pub struct GeneratedSessionBaml {
    pub polymorphic_types: String,
    pub phase_functions: String,
}

/// Enumerates session-plan root functions from compiled IR (tests / tooling seam).
pub trait SessionPlanIrInspector {
    fn for_each_session_plan_binding(&self, f: impl FnMut(&str, Vec<SessionPlanTypeName>));
}

impl SessionPlanIrInspector for IRSignature {
    fn for_each_session_plan_binding(&self, mut f: impl FnMut(&str, Vec<SessionPlanTypeName>)) {
        for (name, func_sig) in &self.functions {
            let plans = crate::builder::baml_signature_gen::session_plan_type_names_from_ir(
                &func_sig.output,
            );
            if !plans.is_empty() {
                f(name.as_str(), plans);
            }
        }
    }
}

fn merge_session_context_into_args_block(base_args: String, has_session_context: bool) -> String {
    if has_session_context {
        base_args
    } else {
        let inner = base_args.trim();
        let before_close = inner.strip_suffix(')').unwrap_or(inner);
        let trimmed = before_close.trim_end();
        if trimmed == "(" {
            "(\n  session_context: SessionContext\n)".to_string()
        } else {
            format!("{trimmed},\n  session_context: SessionContext\n)")
        }
    }
}

/// BAML `( … )` args for generated step executors: IR params plus injected `session_context` when missing.
fn executor_args_block_from_ir(inputs: &[(String, baml_types::TypeIR)]) -> String {
    let base_args = build_args_block_from_ir(inputs);
    let has_session_context = inputs.iter().any(|(name, _)| name == "session_context");
    merge_session_context_into_args_block(base_args, has_session_context)
}

fn legal_union_members_for_unified_primary<T>(
    output: &TypeGeneric<T>,
    cfg: &baml_rt_tools::UnifiedStepExecutorRootConfig,
) -> Vec<String>
where
    T: Clone + std::fmt::Debug,
{
    let mut legal: Vec<String> = collect_union_type_names(output)
        .into_iter()
        .filter(|t| !t.ends_with("SessionPlan"))
        .collect();
    if cfg.include_archive_reads {
        for step in ["ArchiveSearchReadStep", "ArchivePageReadStep"] {
            if !legal.iter().any(|t| t == step) {
                legal.push(step.to_string());
            }
        }
    }
    legal.sort();
    legal.dedup();
    legal
}

fn phase_act_supplement_after_cue(tool_name_str: &str) -> String {
    if tool_name_str == "system/discover_agents" {
        format!(
            "A {tool_name_str} session is open. Emit Send for a new query, or SearchRead/PageRead an existing @N archive from history when listing output is already fetched — do not re-Send the same discover_agents listing without trying SearchRead/PageRead pagination first.\n\n{DISCOVER_AGENTS_SEND_DISCIPLINE}"
        )
    } else {
        format!(
            "A {tool_name_str} session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.\n\n"
        )
    }
}

fn phase_continue_supplement_after_cue(tool_name_str: &str) -> String {
    let mut s = format!(
        "{tool_name_str} result is archived.\n\
         Check session history:\n\
         - See \"@N {tool_name_str}\" followed by numbered lines → content is inline; emit Finish\n\
         - See \"@N {tool_name_str}\" with \"more lines\" indicator → emit SearchRead or PageRead to paginate\n\
         - See \"@N {tool_name_str}\" with no content yet → emit SearchRead or PageRead with archive_ref=\"@N\"\n\
         - Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern\n\n"
    );
    if tool_name_str == "system/discover_agents" {
        s.push_str(DISCOVER_AGENTS_SEND_DISCIPLINE);
        s.push_str(DISCOVER_AGENTS_CONTINUE_FINISH_HINT);
    }
    s
}

/// Generate polymorphic session BAML types AND per-phase step executor functions from the
/// compiled IR. Single source of truth — no source text parsing.
///
/// Returns [`GeneratedSessionBaml`]. The compiler merges both sections into
/// [`super::GENERATED_BAML_PRELUDE_FILE`]. Either string may be empty.
///
/// Must be called after the first `BamlRuntime::from_directory` so the IR is available.
/// A second compilation pass is then needed to include the generated types.
///
/// Phase executor prompts are verbatim copies of each parent function's IR `prompt_template`.
pub fn render_generated_session_baml_from_ir(
    runtime: &baml_runtime::BamlRuntime,
    tool_metadata: &[ToolFunctionMetadata],
    unified_roots: &baml_rt_tools::UnifiedStepExecutorFunctionsMap,
) -> Result<GeneratedSessionBaml> {
    let ir = runtime.ir.deref();

    let ir_sig = IRSignature::new_from_ir(ir).map_err(|e| {
        crate::builder::error::BamlBuilderError::IrSignatureExtraction { source: e }
    })?;

    let tool_by_class: HashMap<&str, &ToolFunctionMetadata> = tool_metadata
        .iter()
        .map(|t| (t.class_name.as_str(), t))
        .collect();

    let mut poly_out = String::new();
    let mut phase_out = String::new();

    write_line(
        &mut phase_out,
        "// Auto-generated per-phase step executor functions.",
    )?;
    write_line(
        &mut phase_out,
        "// Each phase narrows the return type to only the legal FSM ops.",
    )?;
    write_line(&mut phase_out, "")?;
    // SessionContext lives in the shared prelude (`prompt_copy::render_generated_tools_prelude`) so tool
    // coordination BAML can reference it when merged into `_baml_runtime.baml` before this section.

    for func in ir.walk_functions() {
        let func_name = func.name();

        let Some(func_sig) = ir_sig.functions.get(func_name) else {
            continue;
        };
        let plan_types =
            crate::builder::baml_signature_gen::session_plan_type_names_from_ir(&func_sig.output);
        if plan_types.is_empty() {
            continue;
        }

        let mut candidates: Vec<&ToolFunctionMetadata> = plan_types
            .iter()
            .filter_map(|pt| tool_by_class.get(pt.class_name()))
            .copied()
            .collect();
        if candidates.is_empty() {
            continue;
        }
        candidates.sort_by_key(|t| t.name.to_string());

        if candidates.len() > 1 {
            generate_polymorphic_session_baml_for_function(&mut poly_out, func_name, &candidates)?;
            write_line(&mut poly_out, "")?;
        }

        let Some(config) = func.elem().configs.first() else {
            continue;
        };
        let client_name = config.client.as_str();
        let prompt_template = &config.prompt_template;

        let args_block = executor_args_block_from_ir(&func.elem().inputs);

        let non_plan_types: Vec<String> = {
            let all_members = collect_union_type_names(&func_sig.output);
            all_members
                .into_iter()
                .filter(|t| !t.ends_with("SessionPlan"))
                .collect()
        };

        let open_types: Vec<String> = candidates
            .iter()
            .map(|t| SessionTypeNames::open_step(&t.class_name))
            .collect();
        let mut select_return = non_plan_types.clone();
        select_return.extend(open_types);
        let select_name = SessionTypeNames::select(func_name);

        let tool_list = candidates
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let select_supplement = format!("Open a session with: {tool_list}.\n\n");

        write_line(&mut phase_out, "/// Phase: select — open a tool session.")?;
        write_line(
            &mut phase_out,
            &format!(
                "function {select_name}{args_block} -> {} {{",
                select_return.join(" | ")
            ),
        )?;
        let select_suffix = select_phase_executor_suffix(&select_return);
        let select_spec = phase_prompt::ToolSessionPhasePromptSpec {
            phase: phase_prompt::PhaseHop::Select,
            legal_type_names: &select_return,
            constraint_suffix: select_suffix,
            supplement_after_cue: Some(select_supplement.as_str()),
        };
        write_line(
            &mut phase_out,
            &select_spec.emit_baml_prompt_body(client_name.as_str(), prompt_template),
        )?;
        write_line(&mut phase_out, "}")?;
        write_line(&mut phase_out, "")?;

        for tool in &candidates {
            let slug = tool.name.slug();
            let tool_name_str = tool.name.to_string();
            let send_type = format!("{}SendStep", tool.class_name);
            let search_read_type = format!("{}SearchReadStep", tool.class_name);
            let page_read_type = format!("{}PageReadStep", tool.class_name);
            let finish_type = format!("{}FinishStep", tool.class_name);
            let abort_type = format!("{}AbortStep", tool.class_name);

            let act_supplement = phase_act_supplement_after_cue(&tool_name_str);
            let legal_act = vec![
                send_type.clone(),
                search_read_type.clone(),
                page_read_type.clone(),
            ];
            let act_name = SessionTypeNames::act(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!(
                    "/// Phase: act — first post-Open hop: Send, SearchRead, PageRead, or Abort ({tool_name_str})."
                ),
            )?;
            write_line(
                &mut phase_out,
                &format!(
                    "function {act_name}{args_block} -> {send_type} | {search_read_type} | {page_read_type} | {abort_type} {{"
                ),
            )?;
            let act_spec = phase_prompt::ToolSessionPhasePromptSpec {
                phase: phase_prompt::PhaseHop::Act {
                    tool_display_name: tool_name_str.as_str(),
                },
                legal_type_names: &legal_act,
                constraint_suffix: PHASE_STEP_EXECUTOR_SUFFIX_ACT,
                supplement_after_cue: Some(act_supplement.as_str()),
            };
            write_line(
                &mut phase_out,
                &act_spec.emit_baml_prompt_body(client_name.as_str(), prompt_template),
            )?;
            write_line(&mut phase_out, "}")?;
            write_line(&mut phase_out, "")?;

            let continue_supplement = phase_continue_supplement_after_cue(&tool_name_str);
            let legal_continue = vec![
                send_type.clone(),
                search_read_type.clone(),
                page_read_type.clone(),
                finish_type.clone(),
            ];
            let continue_name = SessionTypeNames::r#continue(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!(
                    "/// Phase: continue — SearchRead/PageRead, send again, finish, or abort {tool_name_str}."
                ),
            )?;
            write_line(
                &mut phase_out,
                &format!(
                    "function {continue_name}{args_block} -> {send_type} | {search_read_type} | {page_read_type} | {finish_type} | {abort_type} {{"
                ),
            )?;
            let continue_spec = phase_prompt::ToolSessionPhasePromptSpec {
                phase: phase_prompt::PhaseHop::Continue {
                    tool_display_name: tool_name_str.as_str(),
                },
                legal_type_names: &legal_continue,
                constraint_suffix: PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE,
                supplement_after_cue: Some(continue_supplement.as_str()),
            };
            write_line(
                &mut phase_out,
                &continue_spec.emit_baml_prompt_body(client_name.as_str(), prompt_template),
            )?;
            write_line(&mut phase_out, "}")?;
            write_line(&mut phase_out, "")?;
        }
    }

    append_unified_primary_step_executors(&mut phase_out, runtime, unified_roots, &ir_sig)?;

    if phase_out
        .lines()
        .all(|l| l.starts_with("//") || l.is_empty())
    {
        phase_out.clear();
    }

    Ok(GeneratedSessionBaml {
        polymorphic_types: poly_out,
        phase_functions: phase_out,
    })
}

fn append_unified_primary_step_executors(
    phase_out: &mut String,
    runtime: &baml_runtime::BamlRuntime,
    unified_roots: &baml_rt_tools::UnifiedStepExecutorFunctionsMap,
    ir_sig: &IRSignature,
) -> Result<()> {
    if unified_roots.is_empty() {
        return Ok(());
    }

    let ir = runtime.ir.deref();

    write_line(
        phase_out,
        "// ── builder: unified structured step executors (unified_step_executors.json) ──",
    )?;
    write_line(phase_out, "")?;

    let mut sorted_roots: Vec<&String> = unified_roots.keys().collect();
    sorted_roots.sort();

    for func_name in sorted_roots {
        let cfg = &unified_roots[func_name];
        let Some(func_sig) = ir_sig.functions.get(func_name.as_str()) else {
            tracing::warn!(
                function = %func_name,
                "unified_step_executors.json: function not found in BAML IR — skip"
            );
            continue;
        };

        let Some(ir_func) = ir.walk_functions().find(|f| f.name() == func_name.as_str()) else {
            continue;
        };
        let Some(config) = ir_func.elem().configs.first() else {
            continue;
        };
        let client_name = config.client.as_str();
        let prompt_template = &config.prompt_template;

        let args_block = executor_args_block_from_ir(&ir_func.elem().inputs);

        let legal = legal_union_members_for_unified_primary(&func_sig.output, cfg);
        if legal.is_empty() {
            tracing::warn!(
                function = %func_name,
                "unified step executor: empty legal union after IR harvest — skip"
            );
            continue;
        }

        let union_ty = legal.join(" | ");
        let select_name = SessionTypeNames::select(func_name);

        write_line(
            phase_out,
            &format!(
                "/// Unified structured hop — archive reads, structured output, or AskUser ({func_name})."
            ),
        )?;
        write_line(
            phase_out,
            &format!("function {select_name}{args_block} -> {union_ty} {{"),
        )?;
        write_line(
            phase_out,
            &phase_prompt::phase_executor_prompt_body_unified_primary(
                client_name.as_str(),
                prompt_template,
                &legal,
            ),
        )?;
        write_line(phase_out, "}")?;
        write_line(phase_out, "")?;
    }

    Ok(())
}

/// Render a BAML args block from IR input types: `(name: type, name: type?, ...)`.
fn build_args_block_from_ir(inputs: &[(String, baml_types::TypeIR)]) -> String {
    if inputs.is_empty() {
        return "()".to_string();
    }
    let params: Vec<String> = inputs
        .iter()
        .map(|(name, ty)| format!("{name}: {}", type_ir_to_baml(ty)))
        .collect();
    format!("(\n  {}\n)", params.join(",\n  "))
}

fn generate_polymorphic_session_baml_for_function(
    output: &mut String,
    function_name: &str,
    candidates: &[&ToolFunctionMetadata],
) -> Result<()> {
    let open_step_name = format!("{function_name}OpenStep");
    let plan_name = format!("{function_name}SessionPlan");

    let tool_name_literals: Vec<String> = candidates
        .iter()
        .map(|t| format!("\"{}\"", t.name))
        .collect();
    let tool_name_union = tool_name_literals.join(" | ");

    let open_input_types: Vec<&str> = candidates
        .iter()
        .filter(|t| {
            t.open_input_type.name != "()"
                && t.open_input_type.name != "null"
                && t.open_input_type.name != "void"
                && t.open_input_schema
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .map(|m| !m.is_empty())
                    .unwrap_or(false)
        })
        .map(|t| t.open_input_type.name.as_str())
        .collect();

    let card_names: Vec<String> = candidates
        .iter()
        .map(|t| format!("{}ToolCard", t.class_name))
        .collect();

    write_line(
        output,
        &format!(
            "/// Polymorphic Open step for {function_name}: selects a tool and opens a session."
        ),
    )?;
    write_line(
        output,
        &format!("/// See {} for tool capabilities.", card_names.join(", ")),
    )?;
    write_line(output, &format!("class {open_step_name} {{"))?;
    write_line(output, "  op \"Open\"")?;
    write_line(
        output,
        &format!(
            "  tool_name ({tool_name_union}) @description(\"Which tool to open. See ToolCard classes for capabilities.\")"
        ),
    )?;
    if !open_input_types.is_empty() {
        let input_union = if open_input_types.len() == 1 {
            open_input_types[0].to_string()
        } else {
            format!("({})", open_input_types.join(" | "))
        };
        write_line(
            output,
            &format!(
                "  initial_input {input_union}? @description(\"Optional open payload for the selected tool\")"
            ),
        )?;
    }
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(
        output,
        &format!("/// Session plan for {function_name}: polymorphic tool selection via Open."),
    )?;
    write_line(output, &format!("class {plan_name} {{"))?;
    write_line(
        output,
        &format!(
            "  step {open_step_name} @description(\"Select a tool and emit Open. After this, the session auto-narrows to the selected tool's step executor.\")"
        ),
    )?;
    write_line(
        output,
        "  citations string[] @description(\"History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.\")",
    )?;
    write_line(output, "}")?;

    Ok(())
}

#[cfg(test)]
#[test]
fn select_phase_executor_suffix_open_only_when_all_open_step_named() {
    assert_eq!(
        select_phase_executor_suffix(&["SystemFooOpenStep".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT_OPEN_ONLY
    );
    assert_eq!(
        select_phase_executor_suffix(&["AlphaOpenStep".to_string(), "BetaOpenStep".to_string(),]),
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT_OPEN_ONLY
    );
}

#[cfg(test)]
#[test]
fn select_phase_executor_suffix_branchy_when_non_open_step_present() {
    assert_eq!(
        select_phase_executor_suffix(&[
            "CoordinatorReport".to_string(),
            "SystemFooOpenStep".to_string()
        ]),
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT
    );
    assert_eq!(
        select_phase_executor_suffix(&["AskUser".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT
    );
}

#[cfg(test)]
#[test]
fn select_phase_executor_suffix_open_only_despite_session_plan_sibling_in_ir_simulation() {
    // Both entries are open-shaped step class names; non_plan_types-heuristic would be wrong if it only checked emptiness.
    assert_eq!(
        select_phase_executor_suffix(&["FooOpenStep".to_string(), "BarOpenStep".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT_OPEN_ONLY
    );
}

#[cfg(test)]
#[test]
fn select_phase_executor_suffix_empty_falls_back_to_branchy() {
    assert_eq!(
        select_phase_executor_suffix(&[]),
        PHASE_STEP_EXECUTOR_SUFFIX_SELECT
    );
}

#[cfg(test)]
#[test]
fn discover_agents_send_discipline_requires_paging_before_resend() {
    assert!(
        DISCOVER_AGENTS_SEND_DISCIPLINE.contains("PAGING BEFORE RE-SEND"),
        "expected page-before-resend rule in discover_agents discipline"
    );
}

#[cfg(test)]
#[test]
fn discover_agents_act_preamble_aligns_with_generic_archive_discipline() {
    let tool = "system/discover_agents";
    let act = format!(
        "[ACT] A {tool} session is open. Emit Send for a new query, or SearchRead/PageRead an existing @N archive from history when listing output is already fetched — do not re-Send the same discover_agents listing without trying SearchRead/PageRead pagination first.\\n\\n{DISCOVER_AGENTS_SEND_DISCIPLINE}"
    );
    assert!(
        act.contains("do not re-Send the same discover_agents listing"),
        "expected explicit anti-resend for duplicate listing: {act}"
    );
    assert!(
        act.contains("SearchRead/PageRead"),
        "expected SearchRead/PageRead coupling: {act}"
    );
}
