//! Session plans and per-phase step executors generated from compiled BAML IR.
//!
//! Per-phase functions (`__select`, `__act__*`, `__continue__*`) share the parent session-plan
//! BAML function's `prompt_template`. Each hop wraps that template with a **phase-specific
//! mechanical preamble** (phase facts, legal-op boundary, archive-ref mechanics) and a
//! **phase-constraint suffix** (JSON root shape for that hop). These bookends should not encode
//! business behavior such as freshness, whether to paginate, or whether to reuse cached data; that
//! belongs in the agent-authored prompt or explicit tool metadata. The narrowed return type alone
//! is not enough — the model needs prose — but generated prose must stay aligned with the schema
//! and defer policy decisions to the agent author.

use std::{collections::HashMap, ops::Deref};

use baml_rt_tools::{SessionPlanTypeName, SessionTypeNames, tools::ToolFunctionMetadata};
use internal_baml_core::ir::ir_hasher::IRSignature;

use super::ir_type_print::{collect_union_type_names, type_ir_to_baml};
use crate::builder::error::{Result, write_line};

/// Appended to the shared session-coordination prompt on **select** hops so the model does not
/// emit a step wrapper when the IR return type is a bare Open/read step (parse failure).
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_SELECT: &str = r#"

PHASE CONSTRAINT (select): Emit exactly one JSON value matching this narrowed return schema. Do not emit operations absent from ctx.output_format. If an OpenStep type is present, Open binds a real tool session for fresh tool work. If non-session result types are present, they may be returned according to the agent-authored policy. Archive reads are legal only when SearchRead or PageRead appears in this narrowed schema. Do NOT wrap a bare step under a parent step property unless ctx.output_format requires that wrapper.
"#;

/// Appended on **act** (first post-Open hop: Send or archive paging) — same archive discipline as continue.
const PHASE_STEP_EXECUTOR_SUFFIX_ACT: &str = r#"

PHASE CONSTRAINT (act): Emit exactly one JSON value matching this narrowed return schema. Legal ops in the current schema are Send, SearchRead, PageRead, or Abort. Finish is not legal in this first post-Open phase. Never emit legacy op Read; use SearchRead with non-empty grep for filtered archive evidence, or PageRead for a contiguous archive window. Archive refs use @N, never #N. Do NOT return Open or non-session response types. Do NOT wrap a bare step under a parent step property unless ctx.output_format requires that wrapper.
"#;

/// Appended on **continue** hops (Send | SearchRead | PageRead | Finish | Abort).
const PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE: &str = r#"

PHASE CONSTRAINT (continue): Emit exactly one JSON value matching this narrowed return schema. Legal ops in the current schema are Send, SearchRead, PageRead, Finish, or Abort. Never emit legacy op Read; use SearchRead with non-empty grep for filtered archive evidence, or PageRead for a contiguous archive window. Archive refs use @N, never #N. Do NOT return Open or non-session response types. Do NOT wrap a bare step under a parent step property unless ctx.output_format requires that wrapper.
"#;

/// Injected into **act** and **continue** preambles for `system/discover_agents` only.
/// Models often add `required_capabilities` / subscription filters from inferred intent; those
/// filters are strict server-side and frequently yield zero rows, which bypasses query-only
/// fallback and looks like no agents exist.
const DISCOVER_AGENTS_SEND_DISCIPLINE: &str = r#"DISCOVERY INPUT SHAPE NOTE: For broad listing and routing, the query field is the free-text match over name, package, and description. Use required_capabilities, required_schema_versions, or required_source_kinds only when the agent-authored task policy explicitly requires those filters; these filters are strict and can yield zero rows.

ARCHIVE EVIDENCE NOTE: If a prior discover_agents result is archived at @N, SearchRead or PageRead can inspect that evidence when the agent-authored task policy needs it. Re-send only when fresh or different discovery work is required by policy.

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
        let read_types: Vec<String> = candidates
            .iter()
            .flat_map(|t| {
                [
                    format!("{}SearchReadStep", t.class_name),
                    format!("{}PageReadStep", t.class_name),
                ]
            })
            .collect();
        let mut select_return = non_plan_types.clone();
        select_return.extend(open_types);
        select_return.extend(read_types);
        let select_name = SessionTypeNames::select(func_name);

        let tool_list = candidates
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let select_preamble = format!(
            "[RUNTIME PHASE: SELECT] Entry decision before any tool session is bound.\\n\
             Candidate tools available to Open if fresh tool work is required: {tool_list}.\\n\
             Emit exactly one value from this phase narrowed schema. The agent-authored prompt below owns business policy such as reuse, freshness, clarification, and whether tool work is needed.\\n\
             Archive evidence may be read only if SearchRead or PageRead appears in ctx.output_format for this hop; otherwise choose another legal output.\\n\\n"
        );

        write_line(&mut phase_out, "/// Phase: select — entry decision before a tool session is bound; may Open or read archive evidence.")?;
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
            let abort_type = format!("{}AbortStep", tool.class_name);

            let act_preamble = if tool_name_str == "system/discover_agents" {
                format!(
                    "[RUNTIME PHASE: ACT] A {tool_name_str} session is open; this is the first post-Open hop.\\n\
                     Legal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, or Abort. Finish is not legal in this phase.\\n\
                     Use Send only when the agent-authored policy requires fresh tool work. Use SearchRead or PageRead only when policy requires inspecting archived evidence.\\n\\n{DISCOVER_AGENTS_SEND_DISCIPLINE}"
                )
            } else {
                format!(
                    "[RUNTIME PHASE: ACT] A {tool_name_str} session is open; this is the first post-Open hop.\\n\
                     Legal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, or Abort. Finish is not legal in this phase.\\n\
                     Use Send only when the agent-authored policy requires fresh tool work. Use SearchRead or PageRead only when policy requires inspecting archived evidence.\\n\\n"
                )
            };
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
                    "[RUNTIME PHASE: CONTINUE] A {tool_name_str} operation has produced a Done result.\\n\
                     Legal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, Finish, or Abort.\\n\
                     If visible context is sufficient under the agent-authored policy, Finish. If additional archived evidence is required, use the visible @N handle with SearchRead or PageRead. If fresh or different tool work is required, Send is legal in this phase.\\n\
                     A more-lines indicator means more archive content exists; it is not by itself an instruction to paginate.\\n\\n\
                     {DISCOVER_AGENTS_SEND_DISCIPLINE}"
                )
            } else {
                format!(
                    "[RUNTIME PHASE: CONTINUE] A {tool_name_str} operation has produced a Done result.\\n\
                     Legal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, Finish, or Abort.\\n\
                     If visible context is sufficient under the agent-authored policy, Finish. If additional archived evidence is required, use the visible @N handle with SearchRead or PageRead. If fresh or different tool work is required, Send is legal in this phase.\\n\
                     A more-lines indicator means more archive content exists; it is not by itself an instruction to paginate.\\n\\n"
                )
            };
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
fn discover_agents_send_discipline_defers_policy_to_agent_prompt() {
    assert!(
        DISCOVER_AGENTS_SEND_DISCIPLINE.contains("agent-authored task policy"),
        "generated discovery note should defer reuse/filtering behavior to authored policy"
    );
    assert!(
        !DISCOVER_AGENTS_SEND_DISCIPLINE.contains("PAGING BEFORE RE-SEND"),
        "generated discovery note should not impose a hidden pagination policy"
    );
}

#[cfg(test)]
#[test]
fn discover_agents_act_preamble_is_mechanical_not_behavioral() {
    let tool = "system/discover_agents";
    let act = format!(
        "[RUNTIME PHASE: ACT] A {tool} session is open; this is the first post-Open hop.\\n\
         Legal outputs are exactly the narrowed schema below, typically Send, SearchRead, PageRead, or Abort. Finish is not legal in this phase.\\n\
         Use Send only when the agent-authored policy requires fresh tool work. Use SearchRead or PageRead only when policy requires inspecting archived evidence.\\n\\n{DISCOVER_AGENTS_SEND_DISCIPLINE}"
    );
    assert!(
        act.contains("Legal outputs are exactly the narrowed schema"),
        "expected mechanical legal-op framing: {act}"
    );
    assert!(
        act.contains("agent-authored policy"),
        "expected behavior to be delegated to authored policy: {act}"
    );
    assert!(
        !act.contains("do not re-Send"),
        "generated preamble should not impose anti-resend business policy: {act}"
    );
}
