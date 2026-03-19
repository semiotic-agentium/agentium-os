//! BAML tool interface generation with FSM-aware prompting hints
//!
//! Generates BAML tool interface files with detailed descriptions and examples
//! following Anthropic's best practices for tool use prompting.

use std::collections::{HashMap, HashSet};

use baml_rt_tools::{tool_catalog::resolve_manifest_tools, tools::ToolFunctionMetadata};
use baml_tools_calculator as _;
use serde_json::Value;

use crate::builder::{
    error::{Result, write_line},
    schema_to_baml,
};

fn escape_baml_description(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Generate BAML tool interface file with FSM-aware prompting hints
pub fn render_baml_tool_interfaces(tool_names: &[String]) -> Result<String> {
    // Force link so inventory sees these metadata registrations (regen_fixtures + builder).
    let _ = baml_tools_calculator::support_calculate_metadata;
    let _ = baml_rt_tools_claude::metadata::claude_dev_metadata;
    let _ = baml_tools_system::metadata::system_internal_a2a_metadata;
    #[cfg(feature = "clickup")]
    let _ = baml_tools_clickup::ClickUpTool::new;
    #[cfg(feature = "notion")]
    let _ = baml_tools_notion::NotionTool::new;
    #[cfg(feature = "slack")]
    let _ = baml_tools_slack::SlackTool::new;
    let tool_metadata = resolve_manifest_tools(tool_names)?;

    let mut output = String::new();

    // Header with FSM documentation
    write_line(&mut output, "// Auto-generated tool interfaces")?;
    write_line(
        &mut output,
        "// This file is auto-generated - do not edit manually",
    )?;
    write_line(&mut output, "")?;
    write_line(
        &mut output,
        "// FSM (Finite State Machine) Tool Session Protocol:",
    )?;
    write_line(
        &mut output,
        "// All host tools use a session-based FSM with strict state transitions:",
    )?;
    write_line(
        &mut output,
        "// 1. Open: Must be the FIRST step - opens a tool session",
    )?;
    write_line(
        &mut output,
        "// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.",
    )?;
    write_line(
        &mut output,
        "// 3. Read: Deref a prior Send result. Requires archive_ref (e.g. '@1'). Supports grep/paginate.",
    )?;
    write_line(&mut output, "// 4. Finish: Closes the session gracefully")?;
    write_line(&mut output, "// 5. Abort: Closes the session with an error")?;
    write_line(&mut output, "//")?;
    write_line(&mut output, "// CRITICAL FSM RULES:")?;
    write_line(&mut output, "// - Open MUST come before Send")?;
    write_line(
        &mut output,
        "// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.",
    )?;
    write_line(
        &mut output,
        "// - Read requires archive_ref from a prior Send. Use it to paginate or grep the output.",
    )?;
    write_line(
        &mut output,
        "// - Always Finish or Abort to close the session",
    )?;
    write_line(&mut output, "")?;
    write_line(&mut output, "// Shared standard planning types")?;
    write_line(&mut output, "class StandardAgentPlanStep {")?;
    write_line(&mut output, "  agent_package string")?;
    write_line(&mut output, "  agent_instance_id string")?;
    write_line(&mut output, "  sub_message string")?;
    write_line(&mut output, "}")?;
    write_line(&mut output, "")?;
    write_line(&mut output, "class StandardStructuredPlan {")?;
    write_line(&mut output, "  intent_description string")?;
    write_line(&mut output, "  objective string")?;
    write_line(&mut output, "  plan_steps StandardAgentPlanStep[]")?;
    write_line(&mut output, "}")?;
    write_line(&mut output, "")?;
    write_line(&mut output, "class HistoryContext {")?;
    write_line(&mut output, "  hop int")?;
    write_line(&mut output, "  op string")?;
    write_line(&mut output, "  status string")?;
    write_line(&mut output, "  truncated bool")?;
    write_line(&mut output, "  cursor string?")?;
    write_line(&mut output, "  payload string?")?;
    write_line(&mut output, "}")?;
    write_line(&mut output, "")?;
    write_line(&mut output, "/// Archive deref input for Read steps.")?;
    write_line(
        &mut output,
        "/// archive_ref is required: use the @N ref from the Send result.",
    )?;
    write_line(&mut output, "class ArchiveReadInput {")?;
    write_line(
        &mut output,
        "  archive_ref string @description(\"Required archive ref e.g. '@1'. Use the ref returned by the preceding Send step.\")",
    )?;
    write_line(
        &mut output,
        "  offset int? @description(\"Line offset for pagination (0-based). Omit to start from the beginning.\")",
    )?;
    write_line(
        &mut output,
        "  limit int? @description(\"Maximum lines to return. Omit for default page size.\")",
    )?;
    write_line(
        &mut output,
        "  grep string? @description(\"Optional grep pattern e.g. 'deploy' or '-i deploy' for case-insensitive.\")",
    )?;
    write_line(&mut output, "}")?;
    write_line(&mut output, "")?;
    // --- Domain type generation ---
    //
    // Tools that carry a `baml_decl` (generated by `#[derive(BamlType)]`) emit
    // their type declarations directly. Tools without `baml_decl` fall back to
    // the `schema_to_baml` JSON Schema conversion pipeline.
    let mut emitted_decl_types: HashSet<String> = HashSet::new();
    let mut macro_decls = String::new();

    // Pass 1: collect pre-rendered BAML declarations from BamlType-enabled tools.
    for tool in &tool_metadata {
        if let Some(decl) = &tool.baml_decl {
            // Track the type names from this tool so schema_to_baml skips them.
            emitted_decl_types.insert(tool.input_type.name.clone());
            emitted_decl_types.insert(tool.output_type.name.clone());
            if tool.open_input_type.name != "()" {
                emitted_decl_types.insert(tool.open_input_type.name.clone());
            }

            if !macro_decls.is_empty() {
                macro_decls.push('\n');
            }
            macro_decls.push_str(decl);
            macro_decls.push('\n');
        }
    }

    if !macro_decls.is_empty() {
        write_line(
            &mut output,
            "// Domain types generated from #[derive(BamlType)]",
        )?;
        write_line(&mut output, &macro_decls)?;
    }

    // Pass 2: for tools WITHOUT baml_decl, fall back to schema_to_baml.
    let mut schemas = HashMap::new();
    let mut type_names = HashMap::new();

    for tool in &tool_metadata {
        // Skip tools that already provided their types via baml_decl.
        if tool.baml_decl.is_some() {
            continue;
        }

        extract_nested_schemas(&tool.input_schema, &mut type_names);
        extract_nested_schemas(&tool.output_schema, &mut type_names);
        if tool.open_input_type.name != "()" {
            extract_nested_schemas(&tool.open_input_schema, &mut type_names);
        }

        // Only add schemas for types not already emitted by baml_decl.
        if !emitted_decl_types.contains(&tool.input_type.name) {
            schemas.insert(tool.input_type.name.clone(), tool.input_schema.clone());
            type_names.insert(tool.input_type.name.clone(), tool.input_type.name.clone());
        }

        if !emitted_decl_types.contains(&tool.output_type.name) {
            schemas.insert(tool.output_type.name.clone(), tool.output_schema.clone());
            type_names.insert(tool.output_type.name.clone(), tool.output_type.name.clone());
        }

        if tool.open_input_type.name != "()"
            && !emitted_decl_types.contains(&tool.open_input_type.name)
        {
            schemas.insert(
                tool.open_input_type.name.clone(),
                tool.open_input_schema.clone(),
            );
            type_names.insert(
                tool.open_input_type.name.clone(),
                tool.open_input_type.name.clone(),
            );
        }
    }

    if !schemas.is_empty() {
        let domain_types = schema_to_baml::generate_baml_types_from_schemas(&schemas, &type_names)?;
        if !domain_types.is_empty() {
            write_line(&mut output, "// Domain types generated from JSON schemas")?;
            write_line(&mut output, &domain_types)?;
        }
    }

    for tool in &tool_metadata {
        generate_tool_card_baml(&mut output, tool)?;
        write_line(&mut output, "")?;
        generate_tool_baml_interface(&mut output, tool)?;
        write_line(&mut output, "")?;
    }

    Ok(output)
}

/// Generate a `*ToolCard` BAML class for a single tool.
///
/// Tool cards present tool metadata (name, description, policy, input summary, tags)
/// as BAML classes with literal field values. Used by the LLM during polymorphic Open
/// to understand tool capabilities and make an informed selection.
fn generate_tool_card_baml(output: &mut String, tool: &ToolFunctionMetadata) -> Result<()> {
    let class_name = &tool.class_name;
    let card_name = format!("{class_name}ToolCard");
    let tool_name = tool.name.to_string();
    let description = escape_baml_description(&tool.description);
    let policy = format!("{:?}", tool.session_policy);
    let input_summary = summarize_input_schema(&tool.input_schema);

    write_line(
        output,
        &format!("/// Tool card for {tool_name}: metadata for polymorphic Open selection."),
    )?;
    write_line(output, &format!("class {card_name} {{"))?;
    write_line(output, &format!("  tool_name \"{tool_name}\""))?;
    write_line(output, &format!("  description \"{description}\""))?;
    write_line(output, &format!("  session_policy \"{policy}\""))?;
    write_line(
        output,
        &format!(
            "  input_summary \"{}\"",
            escape_baml_description(&input_summary)
        ),
    )?;
    if !tool.tags.is_empty() {
        let tags_desc = tool.tags.join(", ");
        write_line(
            output,
            &format!("  tags string[] @description(\"Tool tags: {tags_desc}\")"),
        )?;
    }
    write_line(output, "}")?;
    Ok(())
}

/// Summarize a JSON schema's required fields into a compact `{ field: type, ... }` string.
fn summarize_input_schema(schema: &Value) -> String {
    let Some(obj) = schema.as_object() else {
        return "{}".to_string();
    };
    let props = obj.get("properties").and_then(Value::as_object);
    let required: HashSet<&str> = obj
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let Some(props) = props else {
        return "{}".to_string();
    };

    let mut parts: Vec<String> = Vec::new();
    let mut sorted_keys: Vec<&String> = props.keys().collect();
    sorted_keys.sort();
    for key in sorted_keys {
        let prop = &props[key];
        let ty = prop.get("type").and_then(Value::as_str).unwrap_or("any");
        let optional = if required.contains(key.as_str()) {
            ""
        } else {
            "?"
        };
        parts.push(format!("{key}{optional}: {ty}"));
    }
    format!("{{ {} }}", parts.join(", "))
}

/// Generate polymorphic session BAML types AND per-phase step executor functions from the
/// compiled IR. Single source of truth — no source text parsing.
///
/// Returns `(polymorphic_types_baml, phase_functions_baml)`. Caller writes each string to
/// a separate generated file. Either may be empty if there are no polymorphic functions.
///
/// Must be called after the first `BamlRuntime::from_directory` so the IR is available.
/// A second compilation pass is then needed to include the generated types.
pub fn render_generated_session_baml_from_ir(
    runtime: &baml_runtime::BamlRuntime,
    tool_metadata: &[ToolFunctionMetadata],
) -> Result<(String, String)> {
    use std::ops::Deref;

    use baml_rt_tools::SessionTypeNames;
    use internal_baml_core::ir::ir_hasher::IRSignature;

    let ir = runtime.ir.deref();

    // Build IRSignature to reuse session_plan_type_names_from_generic.
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

    for func in ir.walk_functions() {
        let func_name = func.name();

        // Look up the function's output type in the IR signature.
        let Some(func_sig) = ir_sig.functions.get(func_name) else {
            continue;
        };
        let plan_types =
            crate::builder::baml_signature_gen::session_plan_type_names_from_ir(&func_sig.output);
        if plan_types.is_empty() {
            continue; // not a session function
        }

        // Collect tool metadata candidates for this function's plan types.
        let mut candidates: Vec<&ToolFunctionMetadata> = plan_types
            .iter()
            .filter_map(|pt| tool_by_class.get(pt.class_name()))
            .copied()
            .collect();
        if candidates.is_empty() {
            continue; // no matching tools registered
        }
        candidates.sort_by_key(|t| t.name.to_string());

        // Polymorphic union types are only needed when a function may open more than one tool.
        if candidates.len() > 1 {
            generate_polymorphic_session_baml_for_function(&mut poly_out, func_name, &candidates)?;
            write_line(&mut poly_out, "")?;
        }

        // Phase functions are generated for EVERY session function (single- or multi-tool).
        // Each phase narrows the return type to only the ops legal for that FSM state and
        // injects a phase-specific preamble so user prompts carry only goal + context.
        let Some(config) = func.elem().configs.first() else {
            continue;
        };
        let client_name = config.client.as_str();
        let prompt_template = &config.prompt_template;

        // Build the args block from IR inputs: "(name: type, name: type?, ...)"
        let args_block = build_args_block_from_ir(&func.elem().inputs);

        // Identify non-plan return types for the __select phase.
        let non_plan_types: Vec<String> = {
            let all_members = collect_union_type_names(&func_sig.output);
            all_members
                .into_iter()
                .filter(|t| !t.ends_with("SessionPlan"))
                .collect()
        };

        // Phase-specific preamble is injected before the user's goal description.
        // The schema already enforces which ops are legal — the preamble only
        // provides the minimal FSM context needed for the LLM to act correctly.
        let make_body = |preamble: &str| -> String {
            format!("\n  client {client_name}\n  prompt #\"{preamble}{prompt_template}\"#\n")
        };

        // Phase 1: __select — Open steps from all tools + non-plan return types
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
        write_line(&mut phase_out, &make_body(&select_preamble))?;
        write_line(&mut phase_out, "}")?;
        write_line(&mut phase_out, "")?;

        // Per-tool phases
        for tool in &candidates {
            let slug = tool.name.slug();
            let tool_name_str = tool.name.to_string();
            let send_type = format!("{}SendStep", tool.class_name);
            let read_type = format!("{}ReadStep", tool.class_name);
            let finish_type = format!("{}FinishStep", tool.class_name);

            // __act__: session open, must Send. Schema enforces Send-only output.
            let act_preamble = format!(
                "[SEND] A {tool_name_str} session is open. Emit Send with your query.\\n\\n"
            );
            let act_name = SessionTypeNames::act(func_name, &slug);
            write_line(
                &mut phase_out,
                &format!("/// Phase: act — send query to {tool_name_str}."),
            )?;
            write_line(
                &mut phase_out,
                &format!("function {act_name}{args_block} -> {send_type} {{"),
            )?;
            write_line(&mut phase_out, &make_body(&act_preamble))?;
            write_line(&mut phase_out, "}")?;
            write_line(&mut phase_out, "")?;

            // __continue__: Send completed, result archived. LLM reads or finishes.
            // The archive ref appears in session history as "@N {tool_name_str} ...".
            let continue_preamble = format!(
                "[CONTINUE] {tool_name_str} result is archived.\\n\
                 Check session history:\\n\
                 - See \\\"@N {tool_name_str}\\\" followed by numbered lines → content is inline; emit Finish\\n\
                 - See \\\"@N {tool_name_str}\\\" with \\\"more lines\\\" indicator → emit Read to paginate\\n\
                 - See \\\"@N {tool_name_str}\\\" with no content yet → emit Read archive_ref=\\\"@N\\\"\\n\\n"
            );
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
            write_line(&mut phase_out, &make_body(&continue_preamble))?;
            write_line(&mut phase_out, "}")?;
            write_line(&mut phase_out, "")?;
        }
    }

    // If no phase functions were generated, clear the header comment.
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
    use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};

    fn type_ir_to_baml(ty: &baml_types::TypeIR) -> String {
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

/// Collect the names of all top-level union members from a TypeGeneric output type.
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

/// Scan BAML source files for functions whose return type unions reference multiple
/// `*SessionPlan` types, and generate polymorphic Open/SessionPlan types for them.
/// Called BEFORE the BAML runtime compilation so the types are available in the IR.
#[deprecated(note = "Use render_generated_session_baml_from_ir instead")]
pub fn render_polymorphic_session_baml_from_source(
    baml_src_dir: &std::path::Path,
    tool_metadata: &[ToolFunctionMetadata],
) -> Result<String> {
    let mut poly_functions: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for entry in
        std::fs::read_dir(baml_src_dir).map_err(crate::builder::error::BamlBuilderError::Io)?
    {
        let entry = entry.map_err(crate::builder::error::BamlBuilderError::Io)?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("baml") {
            continue;
        }
        let content =
            std::fs::read_to_string(&path).map_err(crate::builder::error::BamlBuilderError::Io)?;

        // Join into single string and find function signatures with their return types.
        // Handles multi-line: "function Foo(\n ...\n) -> A | B {"
        let mut current_func_name: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("function ")
                && let Some(paren) = trimmed.find('(')
            {
                current_func_name = Some(trimmed[9..paren].trim().to_string());
            }
            if let Some(arrow_pos) = trimmed.find("->") {
                let return_part = &trimmed[arrow_pos + 2..];
                let return_part = return_part.trim().trim_end_matches('{').trim();
                let types: Vec<&str> = return_part.split('|').map(|t| t.trim()).collect();
                let plan_types: Vec<String> = types
                    .iter()
                    .filter(|t| t.ends_with("SessionPlan"))
                    .map(|t| t.to_string())
                    .collect();
                if plan_types.len() > 1
                    && let Some(ref func_name) = current_func_name
                {
                    poly_functions.insert(func_name.clone(), plan_types);
                }
                current_func_name = None;
            }
        }
    }

    if poly_functions.is_empty() {
        return Ok(String::new());
    }

    let tool_by_class: HashMap<&str, &ToolFunctionMetadata> = tool_metadata
        .iter()
        .map(|t| (t.class_name.as_str(), t))
        .collect();

    let mut output = String::new();
    for (func_name, plan_types) in &poly_functions {
        let mut candidates: Vec<&ToolFunctionMetadata> = Vec::new();
        for pt in plan_types {
            let class_name = pt.strip_suffix("SessionPlan").unwrap_or(pt);
            if let Some(tool) = tool_by_class.get(class_name) {
                candidates.push(tool);
            }
        }
        if candidates.len() <= 1 {
            continue;
        }
        candidates.sort_by_key(|t| t.name.to_string());
        generate_polymorphic_session_baml_for_function(&mut output, func_name, &candidates)?;
        write_line(&mut output, "")?;
    }

    Ok(output)
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
    write_line(output, "}")?;

    Ok(())
}

fn schema_allows_empty_or_null_open_input(schema: &Value) -> bool {
    match schema {
        Value::Null => true,
        Value::Object(map) => {
            if let Some(any_of) = map.get("anyOf").and_then(Value::as_array)
                && any_of.iter().any(schema_allows_empty_or_null_open_input)
            {
                return true;
            }
            if let Some(one_of) = map.get("oneOf").and_then(Value::as_array)
                && one_of.iter().any(schema_allows_empty_or_null_open_input)
            {
                return true;
            }
            if map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "null")
            {
                return true;
            }

            let type_allows_object = match map.get("type") {
                Some(Value::String(t)) => t == "object",
                Some(Value::Array(types)) => types
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|t| t == "object" || t == "null"),
                Some(_) => false,
                None => map.contains_key("properties") || map.contains_key("required"),
            };
            if !type_allows_object {
                return false;
            }

            let has_required = map
                .get("required")
                .and_then(Value::as_array)
                .map(|arr| !arr.is_empty())
                .unwrap_or(false);
            if has_required {
                return false;
            }

            let min_properties = map
                .get("minProperties")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            min_properties == 0
        }
        _ => false,
    }
}

fn generate_tool_baml_interface(output: &mut String, tool: &ToolFunctionMetadata) -> Result<()> {
    // Use the derived class name from metadata
    let class_name = &tool.class_name;

    // Use the actual type names from metadata
    let open_input_type_name = &tool.open_input_type.name;
    let input_type_name = &tool.input_type.name;

    let open_step_name = format!("{}OpenStep", class_name);
    let send_step_name = format!("{}SendStep", class_name);
    let read_step_name = format!("{}ReadStep", class_name);
    let finish_step_name = format!("{}FinishStep", class_name);
    let abort_step_name = format!("{}AbortStep", class_name);
    let step_union_name = format!("{}SessionStep", class_name);
    let plan_type_name = format!("{}SessionPlan", class_name);
    let access_note = tool
        .access
        .map(|access| format!(" Access: {}.", access))
        .unwrap_or_default();

    let tool_name = tool.name.to_string();
    let is_claude_or_a2a = tool_name.starts_with("claude/")
        || tool_name.contains("/a2a")
        || tool_name.contains("_a2a");

    let send_input_desc = if is_claude_or_a2a {
        "Conversational message payload. Set text to a non-empty string. Never null, never omit text."
    } else {
        "Payload for this step."
    };
    let step_desc = if is_claude_or_a2a {
        "Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send (input.text MUST be non-empty); Send done → Finish or Read @N."
    } else {
        "Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again."
    };

    // Generate distinct step types for each FSM operation.
    // tool_name literal enables polymorphic tool selection — the runtime uses it
    // to resolve which tool session to open when the function returns multiple
    // *SessionPlan types.
    write_line(output, &format!("class {} {{", open_step_name))?;
    write_line(output, "  op \"Open\"")?;
    write_line(
        output,
        &format!("  tool_name \"{tool_name}\" @description(\"Tool to open\")"),
    )?;
    // Skip initial_input for unit types or schemas with no properties (no meaningful open payload).
    let open_schema_has_properties = tool
        .open_input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if open_input_type_name != "()"
        && open_input_type_name != "null"
        && open_input_type_name != "void"
        && open_schema_has_properties
    {
        let open_is_optional = schema_allows_empty_or_null_open_input(&tool.open_input_schema);
        let optional_suffix = if open_is_optional { "?" } else { "" };
        let initial_input_desc = if open_is_optional {
            "Optional open payload."
        } else {
            "Required open payload."
        };
        write_line(
            output,
            &format!(
                "  initial_input {}{} @description(\"{}\")",
                open_input_type_name, optional_suffix, initial_input_desc
            ),
        )?;
    }
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", send_step_name))?;
    write_line(output, "  op \"Send\"")?;
    write_line(
        output,
        &format!(
            "  input {} @description(\"{}\")",
            input_type_name, send_input_desc
        ),
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", read_step_name))?;
    write_line(output, "  op \"Read\"")?;
    write_line(
        output,
        "  input ArchiveReadInput @description(\"Archive ref and optional pagination/grep params.\")",
    )?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", finish_step_name))?;
    write_line(output, "  op \"Finish\"")?;
    write_line(output, "}")?;
    write_line(output, "")?;

    write_line(output, &format!("class {} {{", abort_step_name))?;
    write_line(output, "  op \"Abort\"")?;
    write_line(output, "}")?;
    write_line(output, "")?;

    // Generate union type for all step types
    write_line(
        output,
        &format!(
            "type {} = {} | {} | {} | {} | {}",
            step_union_name,
            open_step_name,
            send_step_name,
            read_step_name,
            finish_step_name,
            abort_step_name
        ),
    )?;
    write_line(output, "")?;

    // Generate session plan with FSM guidance and example. Runtime resolves the tool from the
    // builder-generated manifest mapping (function name -> plan type).
    write_line(output, &format!("class {} {{", plan_type_name))?;
    write_line(
        output,
        &format!(
            "  step {} @description(\"{}{}\")",
            step_union_name, step_desc, access_note
        ),
    )?;
    write_line(output, "}")?;

    Ok(())
}

/// Extract nested schemas from $defs or definitions and add to type_names mapping
/// Parsed BAML function definition from source text.
struct ParsedBamlFunction {
    name: String,
    /// Full args block including parens: "(objective: string, ...)"
    args_block: String,
    /// Non-SessionPlan return types (e.g. "CrmStepResult")
    non_plan_return_types: Vec<String>,
    /// SessionPlan return type names (e.g. "SupportCrmSessionPlan")
    plan_return_types: Vec<String>,
    /// Everything between { and the closing } of the function (client, prompt, etc.)
    body: String,
}

/// Parse a BAML function's args block and body from source text.
/// Finds `function {name}(...) -> ... { BODY }` and extracts the args and body.
fn parse_function_from_source(
    baml_src_dir: &std::path::Path,
    func_name: &str,
) -> Result<Option<ParsedBamlFunction>> {
    for entry in
        std::fs::read_dir(baml_src_dir).map_err(crate::builder::error::BamlBuilderError::Io)?
    {
        let entry = entry.map_err(crate::builder::error::BamlBuilderError::Io)?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("baml") {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("generated_"))
        {
            continue;
        }
        let content =
            std::fs::read_to_string(&path).map_err(crate::builder::error::BamlBuilderError::Io)?;

        let needle = format!("function {func_name}");
        let Some(start) = content.find(&needle) else {
            continue;
        };

        // Find the opening { of the function body
        let after_func = &content[start..];
        let Some(arrow_pos) = after_func.find("->") else {
            continue;
        };
        let after_arrow = &after_func[arrow_pos + 2..];
        let Some(open_brace) = after_arrow.find('{') else {
            continue;
        };

        // Extract return type (between -> and {)
        let return_part = after_arrow[..open_brace].trim();
        let types: Vec<&str> = return_part.split('|').map(|t| t.trim()).collect();
        let plan_types: Vec<String> = types
            .iter()
            .filter(|t| t.ends_with("SessionPlan"))
            .map(|t| t.to_string())
            .collect();
        let non_plan_types: Vec<String> = types
            .iter()
            .filter(|t| !t.ends_with("SessionPlan"))
            .map(|t| t.to_string())
            .collect();

        // Extract args block: from first ( to matching )
        let args_start = after_func.find('(').unwrap_or(0);
        let mut depth = 0;
        let mut args_end = args_start;
        for (i, ch) in after_func[args_start..].chars().enumerate() {
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    args_end = args_start + i + 1;
                    break;
                }
            }
        }
        let args_block = after_func[args_start..args_end].to_string();

        // Extract body: from opening { to matching }
        let body_start_abs = arrow_pos + 2 + open_brace + 1;
        let body_content = &after_func[body_start_abs..];
        let mut brace_depth = 1i32;
        let mut body_end = 0;
        for (byte_offset, ch) in body_content.char_indices() {
            if ch == '{' {
                brace_depth += 1;
            } else if ch == '}' {
                brace_depth -= 1;
                if brace_depth == 0 {
                    body_end = byte_offset;
                    break;
                }
            }
        }
        let body = body_content[..body_end].to_string();

        return Ok(Some(ParsedBamlFunction {
            name: func_name.to_string(),
            args_block,
            non_plan_return_types: non_plan_types,
            plan_return_types: plan_types,
            body,
        }));
    }
    Ok(None)
}

/// Generate per-phase BAML functions for step executor functions.
///
/// For each step executor function (returns >1 SessionPlan types), generates:
/// - `{Name}__select` — Open phase (tool selection or direct result)
/// - `{Name}__act__{tool_slug}` — Send phase per tool
/// - `{Name}__consume__{tool_slug}` — Read phase per tool
/// - `{Name}__continue__{tool_slug}` — Continue phase per tool (Send|Read|Finish)
pub fn render_per_phase_functions(
    baml_src_dir: &std::path::Path,
    tool_metadata: &[ToolFunctionMetadata],
) -> Result<String> {
    // Find polymorphic functions by scanning source for >1 SessionPlan return types,
    // then parse their full body from source text.
    let mut parsed: Vec<ParsedBamlFunction> = Vec::new();

    // Reuse the same source-scanning logic as render_polymorphic_session_baml_from_source
    // to find which functions are polymorphic step executors.
    for entry in
        std::fs::read_dir(baml_src_dir).map_err(crate::builder::error::BamlBuilderError::Io)?
    {
        let entry = entry.map_err(crate::builder::error::BamlBuilderError::Io)?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("baml") {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("generated_"))
        {
            continue;
        }
        let content =
            std::fs::read_to_string(&path).map_err(crate::builder::error::BamlBuilderError::Io)?;
        let mut current_func: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("function ")
                && let Some(paren) = trimmed.find('(')
            {
                current_func = Some(trimmed[9..paren].trim().to_string());
            }
            if let Some(arrow_pos) = trimmed.find("->") {
                let return_part = &trimmed[arrow_pos + 2..];
                let return_part = return_part.trim().trim_end_matches('{').trim();
                let plan_count = return_part
                    .split('|')
                    .filter(|t| t.trim().ends_with("SessionPlan"))
                    .count();
                if plan_count > 1
                    && let Some(ref name) = current_func
                    && let Some(func) = parse_function_from_source(baml_src_dir, name)?
                {
                    parsed.push(func);
                }
                current_func = None;
            }
        }
    }

    if parsed.is_empty() {
        return Ok(String::new());
    }

    let tool_by_class: HashMap<&str, &ToolFunctionMetadata> = tool_metadata
        .iter()
        .map(|t| (t.class_name.as_str(), t))
        .collect();

    let mut output = String::new();
    write_line(
        &mut output,
        "// Auto-generated per-phase step executor functions.",
    )?;
    write_line(
        &mut output,
        "// Each phase narrows the return type to only the legal FSM ops.",
    )?;
    write_line(&mut output, "")?;

    for func in &parsed {
        // Resolve tools from plan type names
        let mut tools: Vec<&ToolFunctionMetadata> = Vec::new();
        for pt in &func.plan_return_types {
            let class_name = pt.strip_suffix("SessionPlan").unwrap_or(pt);
            if let Some(tool) = tool_by_class.get(class_name) {
                tools.push(tool);
            }
        }
        if tools.is_empty() {
            continue;
        }
        tools.sort_by_key(|t| t.name.to_string());

        use baml_rt_tools::SessionTypeNames;

        // Phase 1: __select — Open steps from all tools + non-plan return types
        {
            let open_types: Vec<String> = tools
                .iter()
                .map(|t| SessionTypeNames::open_step(&t.class_name))
                .collect();
            let mut return_types = func.non_plan_return_types.clone();
            return_types.extend(open_types);
            let return_union = return_types.join(" | ");
            let select_name = SessionTypeNames::select(&func.name);

            write_line(
                &mut output,
                "/// Phase: select — choose a tool to open or return a direct result.",
            )?;
            write_line(
                &mut output,
                &format!(
                    "function {select_name}{} -> {} {{",
                    func.args_block, return_union
                ),
            )?;
            write_line(&mut output, &func.body)?;
            write_line(&mut output, "}")?;
            write_line(&mut output, "")?;
        }

        // Per-tool phases
        for tool in &tools {
            let slug = tool.name.slug();
            let send_type = format!("{}SendStep", tool.class_name);
            let read_type = format!("{}ReadStep", tool.class_name);
            let finish_type = format!("{}FinishStep", tool.class_name);

            // Phase 2: __act — Send only
            let act_name = SessionTypeNames::act(&func.name, &slug);
            write_line(
                &mut output,
                &format!("/// Phase: act — send an action to {}.", tool.name),
            )?;
            write_line(
                &mut output,
                &format!("function {act_name}{} -> {} {{", func.args_block, send_type),
            )?;
            write_line(&mut output, &func.body)?;
            write_line(&mut output, "}")?;
            write_line(&mut output, "")?;

            // No __consume__ phase: Send blocks until Done; archive ref returned directly.
            // After Done, FSM enters __continue__ (Read/Send/Finish).

            // Phase 3: __continue — Read | Send | Finish
            let continue_name = SessionTypeNames::r#continue(&func.name, &slug);
            let continue_return = format!("{send_type} | {read_type} | {finish_type}");
            write_line(
                &mut output,
                &format!(
                    "/// Phase: continue — send again, read again, or finish {}.",
                    tool.name
                ),
            )?;
            write_line(
                &mut output,
                &format!(
                    "function {continue_name}{} -> {} {{",
                    func.args_block, continue_return
                ),
            )?;
            write_line(&mut output, &func.body)?;
            write_line(&mut output, "}")?;
            write_line(&mut output, "")?;
        }
    }

    Ok(output)
}

fn extract_nested_schemas(schema: &Value, type_names: &mut HashMap<String, String>) {
    if let Some(schema_obj) = schema.as_object() {
        // Check $defs (JSON Schema 2020-12)
        if let Some(defs) = schema_obj.get("$defs").and_then(|v| v.as_object()) {
            for def_name in defs.keys() {
                type_names.insert(def_name.clone(), def_name.clone());
            }
        }

        // Check definitions (JSON Schema draft-07)
        if let Some(defs) = schema_obj.get("definitions").and_then(|v| v.as_object()) {
            for def_name in defs.keys() {
                type_names.insert(def_name.clone(), def_name.clone());
            }
        }

        for value in schema_obj.values() {
            extract_nested_schemas(value, type_names);
        }
    } else if let Some(schema_array) = schema.as_array() {
        for item in schema_array {
            extract_nested_schemas(item, type_names);
        }
    }
}
