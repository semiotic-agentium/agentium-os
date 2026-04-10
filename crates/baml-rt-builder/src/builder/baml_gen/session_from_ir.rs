//! Session plans and per-phase step executors generated from compiled BAML IR.

use std::{collections::HashMap, ops::Deref};

use baml_rt_tools::{SessionTypeNames, tools::ToolFunctionMetadata};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::error::{Result, write_line};

/// Appended to the shared session-coordination prompt on **select** hops so the model does not
/// emit a step wrapper when the IR return type is a bare Open step (parse failure).
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_SELECT: &str = r#"

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#;

/// Appended on **act** hops — first bound hop may either issue a fresh Send or reuse a prior archive via Read.
const PHASE_STEP_EXECUTOR_SUFFIX_ACT: &str = r#"

PHASE CONSTRAINT (act): The JSON root must be exactly one Send or Read step. Do NOT return Report, AskUser, Open, or Finish. Do NOT use a step wrapper object. Prefer Read when a recent matching @N already covers this scope; use Send only when new upstream retrieval is required.
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

"#;

/// Generate polymorphic session BAML types AND per-phase step executor functions from the
/// compiled IR. Single source of truth — no source text parsing.
///
/// Returns `(polymorphic_types_baml, phase_functions_baml)`. The compiler merges both into
/// [`super::GENERATED_BAML_PRELUDE_FILE`]. Either string may be empty.
///
/// Must be called after the first `BamlRuntime::from_directory` so the IR is available.
/// A second compilation pass is then needed to include the generated types.
pub fn render_generated_session_baml_from_ir(
    runtime: &baml_runtime::BamlRuntime,
    tool_metadata: &[ToolFunctionMetadata],
) -> Result<(String, String)> {
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

        // Build prompt with concatenation so IR prompt text (and phase suffixes) are not
        // reinterpreted by format! — JSON examples may contain `{`/`}`.
        let make_body = |preamble: &str, phase_suffix: &str| -> String {
            let mut s = String::new();
            s.push_str(&format!("\n  client {client_name}\n  prompt #\""));
            s.push_str(preamble);
            s.push_str(prompt_template);
            s.push_str(phase_suffix);
            s.push_str("\"#\n");
            s
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
            &make_body(&select_preamble, PHASE_STEP_EXECUTOR_SUFFIX_SELECT),
        )?;
        write_line(&mut phase_out, "}")?;
        write_line(&mut phase_out, "")?;

        for tool in &candidates {
            let slug = tool.name.slug();
            let tool_name_str = tool.name.to_string();
            let send_type = format!("{}SendStep", tool.class_name);
            let read_type = format!("{}ReadStep", tool.class_name);
            let finish_type = format!("{}FinishStep", tool.class_name);

            let act_preamble = if tool_name_str == "system/discover_agents" {
                format!(
                    "[ACT] A {tool_name_str} session is open. Emit one step: Read to refine a matching @N when available, otherwise Send for a new upstream query.\\n\\n{}",
                    DISCOVER_AGENTS_SEND_DISCIPLINE
                )
            } else {
                format!(
                    "[ACT] A {tool_name_str} session is open. Emit one step: Read to refine a matching @N when available, otherwise Send for a new upstream query.\\n\\n"
                )
            };
            let act_name = SessionTypeNames::act(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!(
                    "/// Phase: act — issue first bound hop for {tool_name_str} (Send or Read)."
                ),
            )?;
            write_line(
                &mut phase_out,
                &format!("function {act_name}{args_block} -> {send_type} | {read_type} {{"),
            )?;
            write_line(
                &mut phase_out,
                &make_body(&act_preamble, PHASE_STEP_EXECUTOR_SUFFIX_ACT),
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
                     - Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\\n\
                     - If a recent @N from the same tool already represents the same upstream query/input scope (for example same IDs, names, filters, or parent resource), prefer Read on that @N before emitting a new Send\\n\
                     - If the user mentions a concrete entity token (name/id), prefer targeted Read with grep on relevant @N before broad retrieval\\n\\n\
                     {}",
                    DISCOVER_AGENTS_SEND_DISCIPLINE
                )
            } else {
                format!(
                    "[CONTINUE] {tool_name_str} result is archived.\\n\
                     Check session history:\\n\
                     - See \\\"@N {tool_name_str}\\\" followed by numbered lines → content is inline; emit Finish\\n\
                     - See \\\"@N {tool_name_str}\\\" with \\\"more lines\\\" indicator → emit Read to paginate\\n\
                     - See \\\"@N {tool_name_str}\\\" with no content yet → emit Read archive_ref=\\\"@N\\\"\\n\
                     - Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\\n\
                     - If a recent @N from the same tool already represents the same upstream query/input scope (for example same IDs, names, filters, or parent resource), prefer Read on that @N before emitting a new Send\\n\
                     - If the user mentions a concrete entity token (name/id), prefer targeted Read with grep on relevant @N before broad retrieval\\n\\n"
                )
            };
            let continue_name = SessionTypeNames::r#continue(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!("/// Phase: continue — read, send again, or finish {tool_name_str}."),
            )?;
            write_line(
                &mut phase_out,
                &format!(
                    "function {continue_name}{args_block} -> {send_type} | {read_type} | {finish_type} {{"
                ),
            )?;
            write_line(
                &mut phase_out,
                &make_body(&continue_preamble, PHASE_STEP_EXECUTOR_SUFFIX_CONTINUE),
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

    Ok((poly_out, phase_out))
}

/// Render a BAML args block from IR input types: `(name: type, name: type?, ...)`.
fn build_args_block_from_ir(inputs: &[(String, baml_types::TypeIR)]) -> String {
    fn type_ir_to_baml(ty: &baml_types::TypeIR) -> String {
        use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};
        match ty {
            TypeGeneric::Primitive(tv, _) => tv.basename().to_string(),
            TypeGeneric::Class { name, .. } | TypeGeneric::Enum { name, .. } => name.clone(),
            TypeGeneric::RecursiveTypeAlias { name, .. } => name.clone(),
            TypeGeneric::Union(u, _) => match u.view() {
                UnionTypeViewGeneric::Optional(inner) => format!("{}?", type_ir_to_baml(inner)),
                UnionTypeViewGeneric::OneOf(variants)
                | UnionTypeViewGeneric::OneOfOptional(variants) => {
                    let parts: Vec<String> = variants.iter().map(|v| type_ir_to_baml(v)).collect();
                    format!("({})", parts.join(" | "))
                }
                _ => "string".to_string(),
            },
            TypeGeneric::List(item, _) => format!("{}[]", type_ir_to_baml(item)),
            TypeGeneric::Literal(lv, _) => {
                use baml_types::LiteralValue;
                match lv {
                    LiteralValue::String(s) => format!("\"{s}\""),
                    LiteralValue::Int(i) => i.to_string(),
                    LiteralValue::Bool(b) => b.to_string(),
                }
            }
            _ => "string".to_string(),
        }
    }

    if inputs.is_empty() {
        return "()".to_string();
    }
    let params: Vec<String> = inputs
        .iter()
        .map(|(name, ty)| format!("{name}: {}", type_ir_to_baml(ty)))
        .collect();
    format!("(\n  {}\n)", params.join(",\n  "))
}

fn collect_union_type_names<T>(ty: &baml_types::ir_type::TypeGeneric<T>) -> Vec<String>
where
    T: Clone + std::fmt::Debug,
{
    use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};
    match ty {
        TypeGeneric::Class { name, .. }
        | TypeGeneric::Enum { name, .. }
        | TypeGeneric::RecursiveTypeAlias { name, .. } => {
            vec![name.clone()]
        }
        TypeGeneric::Union(u, _) => match u.view() {
            UnionTypeViewGeneric::Optional(inner) => collect_union_type_names(inner),
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => variants
                .iter()
                .flat_map(|v| collect_union_type_names(v))
                .collect(),
            _ => vec![],
        },
        _ => vec![],
    }
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
