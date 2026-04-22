//! Session plans and per-phase step executors generated from compiled BAML IR.
//!
//! Per-phase functions (`__select`, `__act__*`, `__continue__*`) share the parent session-plan
//! BAML function's `prompt_template`. Each hop wraps that template with a **phase-specific
//! preamble** (describing the operation allowed — Open, Send-or-Read, Send-or-Read-or-Finish —
//! and teaching the `@N` archive-ref notation) and a **phase-constraint suffix** (restating the
//! legal JSON root shape for that hop). These bookends are essential: without them the LLM
//! regularly emits malformed ops (e.g. `Read` with `archive_ref = "#0"` instead of `"@0"`) that
//! fail schema validation and stall the step-executor loop with no assistant output. The narrowed
//! return type alone is not enough — the model needs the prose as well.

use std::{collections::HashMap, ops::Deref};

use baml_rt_tools::{SessionPlanTypeName, SessionTypeNames, tools::ToolFunctionMetadata};
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

/// Appended on **act** (first post-Open hop: Send or Read) — same archive discipline as continue.
const PHASE_STEP_EXECUTOR_SUFFIX_ACT: &str = r#"

PHASE CONSTRAINT (act — Send or Read): The JSON root must be exactly one Send or Read step: op must be the string Send or Read, plus input and citations fields as required by the schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset) — use this when an aligned @N archive already exists in history instead of re-Sending the same fetch.
"#;

/// Appended on **continue** hops (Send | Read | Finish).
const PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE: &str = r#"

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, Read, or Finish step. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset).
"#;

/// Injected into **act** and **continue** preambles for `system/discover_agents` only.
/// Models often add `required_capabilities` / subscription filters from inferred intent; those
/// filters are strict server-side and frequently yield zero rows, which bypasses query-only
/// fallback and looks like no agents exist.
const DISCOVER_AGENTS_SEND_DISCIPLINE: &str = r#"DISCOVERY INPUT RULE: For broad listing and routing, set only the `query` field (free-text match on name, package, description). Leave `required_capabilities`, `required_schema_versions`, and `required_source_kinds` null or omit them unless the user explicitly asked to filter by capability or event subscription. Do not add filters to narrow a vague intent — that often yields zero agents.

PAGING BEFORE RE-SEND: When a prior Send for this tool already archived an agents listing at @N, prefer Read on that @N (grep/limit/offset) to page or narrow — do not re-Send discover_agents with the same broad query as a substitute for pagination.

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

/// `client` + `prompt #""#` for a step executor. Uses concatenation so IR text is not passed
/// through `format!` — phase preamble, `prompt_template`, and phase suffix may all contain
/// `{` / `}` tokens that a `format!` would misinterpret.
fn phase_executor_prompt_body(
    client_name: &str,
    preamble: &str,
    prompt_template: &str,
    phase_suffix: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("\n  client {client_name}\n  prompt #\""));
    s.push_str(preamble);
    s.push_str(prompt_template);
    s.push_str(phase_suffix);
    s.push_str("\"#\n");
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

        let base_args = build_args_block_from_ir(&func.elem().inputs);
        let args_block = {
            // Host injects `session_context` for step executors. If the hand-written function
            // already declares it (e.g. `SessionContext?` for polymorphic prompts), do not append
            // a second parameter — duplicate names break BAML compile.
            let has_session_context = func
                .elem()
                .inputs
                .iter()
                .any(|(name, _)| name == "session_context");
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
        };

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
        let select_preamble = format!("[OPEN] Open a session with: {tool_list}.\\n\\n");

        write_line(&mut phase_out, "/// Phase: select — open a tool session.")?;
        write_line(
            &mut phase_out,
            &format!(
                "function {select_name}{args_block} -> {} {{",
                select_return.join(" | ")
            ),
        )?;
        write_line(
            &mut phase_out,
            &phase_executor_prompt_body(
                client_name.as_str(),
                &select_preamble,
                prompt_template,
                PHASE_STEP_EXECUTOR_SUFFIX_SELECT,
            ),
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

            let act_preamble = if tool_name_str == "system/discover_agents" {
                format!(
                    "[ACT] A {tool_name_str} session is open. Emit Send for a new query, or Read an existing @N archive from history (grep/limit/offset) when listing output is already fetched — do not re-Send the same discover_agents listing without trying Read/pagination first.\\n\\n{DISCOVER_AGENTS_SEND_DISCIPLINE}"
                )
            } else {
                format!(
                    "[ACT] A {tool_name_str} session is open. Emit Send for new work, or Read an existing @N archive from history (grep/limit/offset) when the data is already fetched — do not re-Send the same listing.\\n\\n"
                )
            };
            let act_name = SessionTypeNames::act(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!(
                    "/// Phase: act — first post-Open hop: Send or SearchRead/PageRead {tool_name_str}."
                ),
            )?;
            write_line(
                &mut phase_out,
                &format!(
                    "function {act_name}{args_block} -> {send_type} | {search_read_type} | {page_read_type} {{"
                ),
            )?;
            write_line(
                &mut phase_out,
                &phase_executor_prompt_body(
                    client_name.as_str(),
                    &act_preamble,
                    prompt_template,
                    PHASE_STEP_EXECUTOR_SUFFIX_ACT,
                ),
            )?;
            write_line(&mut phase_out, "}")?;
            write_line(&mut phase_out, "")?;

            let continue_preamble = if tool_name_str == "system/discover_agents" {
                format!(
                    "[CONTINUE] {tool_name_str} result is archived.\\n\
                     Check session history:\\n\
                     - See \\\"@N {tool_name_str}\\\" followed by numbered lines → content is inline; emit Finish\\n\
                     - See \\\"@N {tool_name_str}\\\" with \\\"more lines\\\" indicator → emit Read to paginate\\n\
                     - See \\\"@N {tool_name_str}\\\" with no content yet → emit Read archive_ref=\\\"@N\\\"\\n\
                     - Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\\n\\n\
                     {DISCOVER_AGENTS_SEND_DISCIPLINE}"
                )
            } else {
                format!(
                    "[CONTINUE] {tool_name_str} result is archived.\\n\
                     Check session history:\\n\
                     - See \\\"@N {tool_name_str}\\\" followed by numbered lines → content is inline; emit Finish\\n\
                     - See \\\"@N {tool_name_str}\\\" with \\\"more lines\\\" indicator → emit Read to paginate\\n\
                     - See \\\"@N {tool_name_str}\\\" with no content yet → emit Read archive_ref=\\\"@N\\\"\\n\
                     - Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\\n\\n"
                )
            };
            let continue_name = SessionTypeNames::r#continue(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!(
                    "/// Phase: continue — SearchRead/PageRead, send again, or finish {tool_name_str}."
                ),
            )?;
            write_line(
                &mut phase_out,
                &format!(
                    "function {continue_name}{args_block} -> {send_type} | {search_read_type} | {page_read_type} | {finish_type} {{"
                ),
            )?;
            write_line(
                &mut phase_out,
                &phase_executor_prompt_body(
                    client_name.as_str(),
                    &continue_preamble,
                    prompt_template,
                    PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE,
                ),
            )?;
            write_line(&mut phase_out, "}")?;
            write_line(&mut phase_out, "")?;
        }
    }

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
        "[ACT] A {tool} session is open. Emit Send for a new query, or Read an existing @N archive from history (grep/limit/offset) when listing output is already fetched — do not re-Send the same discover_agents listing without trying Read/pagination first.\\n\\n{DISCOVER_AGENTS_SEND_DISCIPLINE}"
    );
    assert!(
        act.contains("do not re-Send the same discover_agents listing"),
        "expected explicit anti-resend for duplicate listing: {act}"
    );
    assert!(
        act.contains("Read/pagination"),
        "expected Read/pagination coupling: {act}"
    );
}
