// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashSet, fmt, fs, path::Path};

use baml_rt_tools::{UnifiedStepExecutorFunctionsMap, ts_gen::render_tool_typescript};
use genco::{fmt::Error as GencoFmtError, lang::js, prelude::*};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::{
    error::{BamlBuilderError, Result},
    ir_to_ts::{collect_type_decl_deps, emit_type_declarations_tokens, type_to_ts_expr},
};

/// Wrapper so genco fmt errors can be used as [`std::error::Error`] source.
/// genco's `fmt::Error` does not implement `Error`; this preserves the chain.
#[derive(Debug)]
struct GencoRenderError(GencoFmtError);

impl fmt::Display for GencoRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for GencoRenderError {}

pub fn load_manifest_tools(baml_src: &Path) -> Result<Vec<String>> {
    let agent_dir = baml_src.parent().ok_or_else(|| {
        BamlBuilderError::InvalidArgument("baml_src has no parent directory".to_string())
    })?;
    let manifest_path = agent_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&manifest_path)?;
    let manifest_json: serde_json::Value =
        serde_json::from_str(&content).map_err(BamlBuilderError::Json)?;
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
pub fn render_ts_declarations(
    ir_signature: &IRSignature,
    tool_names: &[String],
    session_plan_functions: &baml_rt_tools::SessionPlanFunctionsMap,
    unified_step_executors: &UnifiedStepExecutorFunctionsMap,
) -> Result<String> {
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
        quote_in!(tokens =>   declare function $(name)(args: $(args_type) & { __baml_invocation_token?: string }): Promise<$(return_type)>;);
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
    tokens.line();

    if !session_plan_functions.is_empty() {
        let session_runner_comment = "/** Generated Step Executor bindings (function -> typed step-executor args/result). */";
        quote_in!(tokens => $(session_runner_comment));
        tokens.line();

        // Stable output for snapshots.
        let mut step_executor_names: Vec<String> = session_plan_functions.keys().cloned().collect();
        for k in unified_step_executors.keys() {
            if !step_executor_names.iter().any(|e| e == k) {
                step_executor_names.push(k.clone());
            }
        }
        step_executor_names.sort();
        let step_executor_union = step_executor_names
            .iter()
            .map(|name| format!("\"{}\"", name))
            .collect::<Vec<_>>()
            .join(" | ");
        quote_in!(tokens => export type StepExecutorFunctionName = $(step_executor_union););
        tokens.line();
        quote_in!(
            tokens =>
            export interface SessionContext {
                contract_version: "session_context_v2";
                session_open: boolean;
                status: "awaiting_open" | "just_opened" | "done";
                last_step_op?: "open" | "send" | "read" | "finish" | "abort";
                last_step_status?: "open" | "done" | "finished" | "aborted";
                last_archive_ref?: string;
                last_output_header?: string;
                last_completion?: string;
            }
        );
        tokens.line();
        let history_ctx_op_comment = "/** Last archive read op for this hop (`StepExecutorStateInput.history_context`); distinct from tool-session `SessionStepOp` in Rust provenance. */";
        quote_in!(tokens => $(history_ctx_op_comment));
        tokens.line();
        quote_in!(
            tokens =>
            export type HistoryContextSessionOp = "SearchRead" | "PageRead";
        );
        tokens.line();
        quote_in!(
            tokens =>
            export type HistoryContextStatus = "done" | "streaming" | "suspended" | "error";
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface HistoryContext {
                hop: number;
                op: HistoryContextSessionOp;
                status: HistoryContextStatus;
                truncated: boolean;
                cursor: string | null;
                payload: Record<string, unknown> | null;
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface StepExecutorStateInput {
                session_context?: SessionContext | null;
                history_context?: HistoryContext | null;
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface StepExecutorRunOptions {
                max_steps?: number;
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export type ErrorDisposition =
                | "host_retriable"
                | "llm_correctable"
                | "inform_and_continue"
                | "fatal";
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface StepPlanRecovery {
                code: string;
                disposition: ErrorDisposition;
                mistake: string;
                invariant: string;
                fix_steps?: string[];
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export type StepExecutorRunEnvelope<R = unknown> = { outcome: "completed", last: R, steps: R[], session_context: SessionContext, selected_tool: string | null } | { outcome: "agent_correctable", recovery: StepPlanRecovery } | { outcome: "fatal", message: string, code?: string | null };
        );
        tokens.line();
        let step_run_result_comment = "/**\n * Result of runGeneratedStepExecutor: discriminated envelope (`outcome`).\n * On `completed`, fields match the former flat telemetry shape.\n * `agent_correctable` carries structured recovery — not a thrown JS error.\n * User-facing replies are still SessionResult.message from the chat handler.\n */";
        quote_in!(tokens => $(step_run_result_comment));
        tokens.line();
        quote_in!(
            tokens =>
            export type StepExecutorRunResult<R = unknown> = StepExecutorRunEnvelope<R>;
        );
        tokens.line();
        let map_open = "export interface StepExecutorFunctionMap {";
        quote_in!(tokens => $(map_open));
        tokens.push();
        for name in &step_executor_names {
            let line = format!(
                "  {}: {{ args: Parameters<typeof {}>[0] & StepExecutorStateInput; result: Awaited<ReturnType<typeof {}>>; }};",
                name, name, name
            );
            quote_in!(tokens => $(line));
            tokens.push();
        }
        let map_close = "}";
        quote_in!(tokens => $(map_close));
        tokens.push();
        tokens.line();
        let global_open = "declare global {";
        quote_in!(tokens => $(global_open));
        tokens.push();
        let step_fn_l1 = "  function runGeneratedStepExecutor<F extends StepExecutorFunctionName>(";
        quote_in!(tokens => $(step_fn_l1));
        tokens.push();
        let step_fn_l2 = "    stepExecutor: F,";
        quote_in!(tokens => $(step_fn_l2));
        tokens.push();
        let step_fn_l3 =
            "    args: Omit<StepExecutorFunctionMap[F][\"args\"], keyof StepExecutorStateInput>,";
        quote_in!(tokens => $(step_fn_l3));
        tokens.push();
        let step_fn_l4 = "    options?: StepExecutorRunOptions";
        quote_in!(tokens => $(step_fn_l4));
        tokens.push();
        let step_fn_l5 =
            "  ): Promise<StepExecutorRunEnvelope<StepExecutorFunctionMap[F][\"result\"]>>;";
        quote_in!(tokens => $(step_fn_l5));
        tokens.push();
        let global_close = "}";
        quote_in!(tokens => $(global_close));
        tokens.push();
        tokens.line();
    }

    tokens
        .to_file_string()
        .map_err(|e| BamlBuilderError::InvalidArgumentWithSource {
            message: "TypeScript render error".into(),
            source: Box::new(GencoRenderError(e)),
        })
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
