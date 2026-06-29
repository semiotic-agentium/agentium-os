// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Session plans and per-phase step executors generated from compiled BAML IR.
//!
//! Per-phase functions (`__entry`, `__active__*`) share the parent session-plan
//! BAML function's `prompt_template`. Each hop is wrapped for BAML with a byte-stable prefix
//! (`SESSION_STEP_STABLE_PREFIX_BAML` + stable `tool_schema_prelude` + canonical session
//! history block), then post-history task text, a compact type-reference contract, the generated
//! selection hint, and a compact state-indexed phase policy. No phase cue, inline union schema
//! dump, or tool-specific narrative is allowed before `Session history`.
//!
//! **Phase policy ↔ generated return types (tool phases):**
//! - **entry** — [`entry_phase_executor_suffix`] must agree with the `function __entry -> …` union.
//! - **active** — [`PHASE_STEP_EXECUTOR_SUFFIX_ACTIVE`] matches `Send | SearchRead | PageRead | Finish | Abort`.

pub mod catalog;
pub(crate) mod phase_prompt;

use std::{collections::HashMap, ops::Deref};

use baml_rt_tools::{
    SessionPlanTypeName, SessionTypeNames, entry_send_eligible, tools::ToolFunctionMetadata,
};
use baml_types::ir_type::TypeGeneric;
pub use catalog::{CATALOG_FUNCTION_NAME, CATALOG_SIDECAR_FILE, CatalogPlan};
use internal_baml_core::ir::ir_hasher::IRSignature;

use super::ir_type_print::{collect_union_type_names, type_ir_to_baml};
use crate::builder::error::{Result, write_line};

/// Compact state-indexed policy for **entry** hops with archive/read-only choices only.
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_ENTRY: &str = r#"

Phase policy:
- Derived state rule: this entry return union excludes `Send`, `Finish`, and `Abort`.
"#;

/// Compact state-indexed policy for **entry** hops that include typed `<Tool>SendStep` variants.
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_WITH_SEND: &str = r#"

Phase policy:
- Derived state rule: eligible one-shot tools may emit Send directly (runtime auto-opens and auto-finishes); Open remains valid during migration. Finish and Abort are excluded on entry.
"#;

/// Compact state-indexed policy for **entry** hops when the union is open-only.
///
/// No ASCII double quotes inside: text is concatenated into BAML `prompt #""#` literals.
const PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_OPEN_ONLY: &str = r#"

Phase policy:
- Derived state rule: this entry return union only allows `Open`.
"#;

/// Per-hop suffix for generated `__entry` functions: Send-inclusive, open-only, or branchy.
///
/// Send-inclusive when any member ends with `SendStep`. Open-only when **every** tool step ends
/// with `OpenStep` (no Send variants). Otherwise branchy (archive reads / non-tool steps only).
pub(crate) fn entry_phase_executor_suffix(entry_return: &[String]) -> &'static str {
    if entry_return.is_empty() {
        return PHASE_STEP_EXECUTOR_SUFFIX_ENTRY;
    }
    if entry_return.iter().any(|t| t.ends_with("SendStep")) {
        return PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_WITH_SEND;
    }
    if entry_return.iter().all(|t| t.ends_with("OpenStep")) {
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_OPEN_ONLY
    } else {
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY
    }
}

/// Appended on **active** hops (after Open): Send | reads | Finish | Abort.
const PHASE_STEP_EXECUTOR_SUFFIX_ACTIVE: &str = r#"

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
- After Send completes with status done, use PageRead or SearchRead on last_archive_ref when archive lines are needed; otherwise emit Finish.
- Never re-Send identical input after a Done; re-Send only for pagination (has_more) or a genuinely different query.
"#;

/// Bundle emitted from IR: polymorphic Open/plan classes, per-phase executor functions,
/// and the stable agent-wide tool / operation catalog used to render `tool_schema_prelude`.
#[derive(Debug, Default, Clone)]
pub struct GeneratedSessionBaml {
    pub polymorphic_types: String,
    pub phase_functions: String,
    /// IR-derived stable catalog text for `ctx.tags['tool_schema_prelude']`.
    pub catalog_plan: CatalogPlan,
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

/// Build the sorted, deduplicated entry-hop return union for a session-plan executor.
fn entry_return_type_names(
    candidates: &[&ToolFunctionMetadata],
    non_plan_types: &[String],
) -> Vec<String> {
    let open_types: Vec<String> = candidates
        .iter()
        .map(|t| SessionTypeNames::open_step(&t.class_name))
        .collect();
    let send_types: Vec<String> = candidates
        .iter()
        .filter(|t| entry_send_eligible(t))
        .map(|t| SessionTypeNames::send_step(&t.class_name))
        .collect();
    let mut entry_return = non_plan_types.to_vec();
    entry_return.extend(open_types);
    entry_return.extend(send_types);
    for extra in [
        "ArchiveSearchReadStep",
        "ArchivePageReadStep",
        "ReadOnlyFinishStep",
    ] {
        if !entry_return.iter().any(|t| t == extra) {
            entry_return.push(extra.to_string());
        }
    }
    entry_return.sort();
    entry_return.dedup();
    entry_return
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
/// Phase executor prompts start from each parent function's IR `prompt_template`, stripped inside
/// `phase_prompt` when composing each hop (see `strip_phase_executor_ir_template`), then wrapped per
/// module docs above.
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

        let entry_return = entry_return_type_names(&candidates, &non_plan_types);
        let entry_name = SessionTypeNames::entry(func_name);

        write_line(
            &mut phase_out,
            "/// Entry step executor: stable prefix, transcript, task body, compact contract, and phase policy.",
        )?;
        write_line(
            &mut phase_out,
            &format!(
                "function {entry_name}{args_block} -> {} {{",
                entry_return.join(" | ")
            ),
        )?;
        let entry_suffix = entry_phase_executor_suffix(&entry_return);
        let entry_spec = phase_prompt::ToolSessionPhasePromptSpec {
            legal_type_names: &entry_return,
            ir_signature: &ir_sig,
            phase_policy: entry_suffix,
        };
        write_line(
            &mut phase_out,
            &entry_spec.emit_baml_prompt_body(client_name.as_str(), prompt_template),
        )?;
        write_line(&mut phase_out, "}")?;
        write_line(&mut phase_out, "")?;

        for tool in &candidates {
            let slug = tool.name.slug();
            let send_type = format!("{}SendStep", tool.class_name);
            let search_read_type = format!("{}SearchReadStep", tool.class_name);
            let page_read_type = format!("{}PageReadStep", tool.class_name);
            let finish_type = format!("{}FinishStep", tool.class_name);
            let abort_type = format!("{}AbortStep", tool.class_name);

            let legal_active = vec![
                send_type.clone(),
                search_read_type.clone(),
                page_read_type.clone(),
                finish_type.clone(),
                abort_type.clone(),
            ];
            let active_name = SessionTypeNames::active(func_name, &slug);
            write_line(
                &mut phase_out,
                "/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.",
            )?;
            write_line(
                &mut phase_out,
                &format!(
                    "function {active_name}{args_block} -> {send_type} | {search_read_type} | {page_read_type} | {finish_type} | {abort_type} {{"
                ),
            )?;
            let active_spec = phase_prompt::ToolSessionPhasePromptSpec {
                legal_type_names: &legal_active,
                ir_signature: &ir_sig,
                phase_policy: PHASE_STEP_EXECUTOR_SUFFIX_ACTIVE,
            };
            write_line(
                &mut phase_out,
                &active_spec.emit_baml_prompt_body(client_name.as_str(), prompt_template),
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

    let catalog_plan =
        catalog::collect_catalog_types(&ir_sig, runtime.ir.deref(), tool_metadata, unified_roots);

    Ok(GeneratedSessionBaml {
        polymorphic_types: poly_out,
        phase_functions: phase_out,
        catalog_plan,
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
        let entry_name = SessionTypeNames::entry(func_name);

        write_line(
            phase_out,
            &format!("/// Unified structured step executor ({func_name})."),
        )?;
        write_line(
            phase_out,
            &format!("function {entry_name}{args_block} -> {union_ty} {{"),
        )?;
        write_line(
            phase_out,
            &phase_prompt::phase_executor_prompt_body_unified_primary(
                client_name.as_str(),
                prompt_template,
                &legal,
                ir_sig,
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
fn entry_phase_executor_suffix_open_only_when_all_open_step_named() {
    assert_eq!(
        entry_phase_executor_suffix(&["SystemFooOpenStep".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_OPEN_ONLY
    );
    assert_eq!(
        entry_phase_executor_suffix(&["AlphaOpenStep".to_string(), "BetaOpenStep".to_string(),]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_OPEN_ONLY
    );
}

#[cfg(test)]
#[test]
fn entry_phase_executor_suffix_branchy_when_non_open_step_present() {
    assert_eq!(
        entry_phase_executor_suffix(&[
            "CoordinatorReport".to_string(),
            "SystemFooOpenStep".to_string()
        ]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY
    );
    assert_eq!(
        entry_phase_executor_suffix(&["AskUser".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY
    );
}

#[cfg(test)]
#[test]
fn entry_phase_executor_suffix_send_inclusive_when_send_step_present() {
    assert_eq!(
        entry_phase_executor_suffix(&[
            "SupportCalculateOpenStep".to_string(),
            "SupportCalculateSendStep".to_string(),
            "ArchiveSearchReadStep".to_string(),
        ]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_WITH_SEND
    );
    assert_eq!(
        entry_phase_executor_suffix(&["SupportSlackNotifySendStep".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_WITH_SEND
    );
}

#[cfg(test)]
#[test]
fn entry_phase_executor_suffix_open_only_despite_session_plan_sibling_in_ir_simulation() {
    // Both entries are open-shaped step class names; non_plan_types-heuristic would be wrong if it only checked emptiness.
    assert_eq!(
        entry_phase_executor_suffix(&["FooOpenStep".to_string(), "BarOpenStep".to_string()]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY_OPEN_ONLY
    );
}

#[cfg(test)]
#[test]
fn entry_phase_executor_suffix_empty_falls_back_to_branchy() {
    assert_eq!(
        entry_phase_executor_suffix(&[]),
        PHASE_STEP_EXECUTOR_SUFFIX_ENTRY
    );
}

#[cfg(test)]
mod entry_return_tests {
    use baml_rt_tools::tools::{ToolCapability, ToolFunctionMetadata, ToolTypeSpec};
    use serde_json::json;

    use super::entry_return_type_names;

    fn sample_tool(
        class_name: &str,
        capability: ToolCapability,
        open_input_schema: serde_json::Value,
    ) -> ToolFunctionMetadata {
        ToolFunctionMetadata {
            name: baml_rt_tools::ToolName::parse("support/sample").expect("valid tool name"),
            class_name: class_name.to_string(),
            description: "sample".to_string(),
            open_input_schema,
            input_schema: json!({}),
            output_schema: json!({}),
            open_input_type: ToolTypeSpec {
                name: "()".to_string(),
                ts_decl: None,
            },
            input_type: ToolTypeSpec {
                name: "SupportSampleInput".to_string(),
                ts_decl: None,
            },
            output_type: ToolTypeSpec {
                name: "SupportSampleOutput".to_string(),
                ts_decl: None,
            },
            baml_decl: None,
            extra_ts_decls: Vec::new(),
            access: None,
            tags: Vec::new(),
            secret_requests: Vec::new(),
            config: None,
            config_bundle: None,
            origin: baml_rt_tools::ToolOrigin::Host,
            backend: baml_rt_tools::ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: baml_rt_tools::SessionPolicy::default(),
            capability,
            event_sources: Vec::new(),
            coordination_baml: None,
        }
    }

    #[test]
    fn eligible_one_shot_gets_open_and_send_on_entry() {
        let tool = sample_tool("SupportCalculate", ToolCapability::OneShot, json!({}));
        let entry_return = entry_return_type_names(&[&tool], &[]);
        assert!(entry_return.contains(&"SupportCalculateOpenStep".to_string()));
        assert!(entry_return.contains(&"SupportCalculateSendStep".to_string()));
        assert!(entry_return.contains(&"ArchiveSearchReadStep".to_string()));
        assert!(entry_return.contains(&"ArchivePageReadStep".to_string()));
        assert!(entry_return.contains(&"ReadOnlyFinishStep".to_string()));
    }

    #[test]
    fn streaming_tool_gets_open_only_not_send_on_entry() {
        let tool = sample_tool("McpGrafanaQuery", ToolCapability::Streaming, json!({}));
        let entry_return = entry_return_type_names(&[&tool], &[]);
        assert!(entry_return.contains(&"McpGrafanaQueryOpenStep".to_string()));
        assert!(!entry_return.iter().any(|t| t.ends_with("SendStep")));
    }

    #[test]
    fn archive_reads_and_read_only_finish_always_present() {
        let tool = sample_tool("SupportCalculate", ToolCapability::OneShot, json!({}));
        let entry_return = entry_return_type_names(&[&tool], &[]);
        for shared in [
            "ArchiveSearchReadStep",
            "ArchivePageReadStep",
            "ReadOnlyFinishStep",
        ] {
            assert!(
                entry_return.contains(&shared.to_string()),
                "entry union must always include {shared}: {entry_return:?}"
            );
        }
    }

    #[test]
    fn archive_reads_survive_when_non_plan_types_present() {
        let tool = sample_tool("SupportCalculate", ToolCapability::OneShot, json!({}));
        let entry_return = entry_return_type_names(&[&tool], &["CoordinatorReport".to_string()]);
        assert!(entry_return.contains(&"CoordinatorReport".to_string()));
        assert!(entry_return.contains(&"ArchiveSearchReadStep".to_string()));
        assert!(entry_return.contains(&"ArchivePageReadStep".to_string()));
        assert!(entry_return.contains(&"ReadOnlyFinishStep".to_string()));
    }
}
