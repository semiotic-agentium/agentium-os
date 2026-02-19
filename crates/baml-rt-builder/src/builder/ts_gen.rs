use std::{collections::HashSet, fs, path::Path};

use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::ts_gen::render_tool_typescript;
use genco::{lang::js, prelude::*};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::ir_to_ts::{
    collect_type_decl_deps, emit_type_declarations_tokens, type_to_ts_expr,
};

pub fn load_manifest_tools(baml_src: &Path) -> Result<Vec<String>> {
    let agent_dir = baml_src.parent().ok_or_else(|| {
        BamlRtError::InvalidArgument("baml_src has no parent directory".to_string())
    })?;
    let manifest_path = agent_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&manifest_path).map_err(BamlRtError::Io)?;
    let manifest_json: serde_json::Value =
        serde_json::from_str(&content).map_err(BamlRtError::Json)?;
    let tools = manifest_json
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(tools)
}

/// Generate TypeScript declarations for BAML runtime: typed function signatures and supporting types.
pub fn render_ts_declarations(ir_signature: &IRSignature, tool_names: &[String]) -> Result<String> {
    let header = "/**
 * BAML runtime TypeScript declarations.
 * Auto-generated from BAML runtime IR — do not edit manually.
 * Use these types and function declarations in your agent code (e.g. index.ts).
 */";
    let mut tokens: js::Tokens = quote!($(header));
    tokens.line();

    let mut all_type_deps: HashSet<String> = HashSet::new();
    let mut func_decls: Vec<(String, String, String)> = Vec::new();
    for (name, func_sig) in &ir_signature.functions {
        let args_frag = build_args_frag(func_sig, ir_signature)?;
        let return_frag = type_to_ts_expr(func_sig.output.as_ref(), ir_signature)?;
        all_type_deps.extend(collect_type_decl_deps(&args_frag));
        all_type_deps.extend(collect_type_decl_deps(&return_frag));
        func_decls.push((name.clone(), args_frag.expr, return_frag.expr));
    }
    func_decls.sort_by(|a, b| a.0.cmp(&b.0));

    let type_decls_comment =
        "/** Types for BAML function arguments and return values (classes, enums, aliases). */";
    quote_in!(tokens => $(type_decls_comment));
    tokens.line();
    let type_decls_tokens = emit_type_declarations_tokens(ir_signature, &all_type_deps)?;
    quote_in!(tokens => $(type_decls_tokens));
    tokens.line();

    let global_comment = "/** BAML functions: call these from your agent (e.g. await MyFunction(args)). Declared in global scope so they are visible when this file is used as a module. */";
    quote_in!(tokens => $(global_comment));
    tokens.line();
    let global_open = "declare global {";
    let global_close = "}";
    quote_in!(tokens => $(global_open));
    tokens.line();
    for (name, args_type, return_type) in func_decls {
        quote_in!(tokens =>   declare function $(name)(args: $(args_type)): Promise<$(return_type)>;);
        tokens.line();
    }
    quote_in!(tokens => $(global_close));
    tokens.line();

    let tool_comment =
        "/** Runtime interaction API: A2A task FSM (message-first, typestate rails). */";
    quote_in!(tokens => $(tool_comment));
    tokens.line();
    let _ = tool_names;
    let tool_ts = render_tool_typescript(&[])?;
    for line in tool_ts.lines() {
        quote_in!(tokens => $(line));
        tokens.push();
    }

    tokens
        .to_file_string()
        .map_err(|e| BamlRtError::InvalidArgument(format!("TypeScript render error: {e}")))
}

/// Build an object type for function args: { name1: Type1; name2: Type2; ... }.
fn build_args_frag(
    func_sig: &internal_baml_core::ir::ir_hasher::FunctionSignature,
    ir: &IRSignature,
) -> Result<crate::builder::ir_to_ts::TsTypeFrag> {
    use crate::builder::ir_to_ts::TsTypeFrag;
    if func_sig.inputs.is_empty() {
        return Ok(TsTypeFrag {
            expr: "Record<string, never>".to_string(),
            deps: vec![],
        });
    }
    let mut parts = Vec::with_capacity(func_sig.inputs.len());
    let mut deps = Vec::new();
    for (arg_name, arg_ty) in func_sig.inputs.iter() {
        let frag = type_to_ts_expr(arg_ty, ir)?;
        deps.extend(frag.deps);
        parts.push(format!("{}: {}", arg_name, frag.expr));
    }
    let expr = format!("{{ {} }}", parts.join("; "));
    Ok(TsTypeFrag { expr, deps })
}
