//! Agent-wide tool schema catalog generation.
//!
//! Emits a synthetic BAML function `__AgentToolSchemaCatalog__` whose return type is the union of
//! every tool/session step type the agent can use across Open / Send / SearchRead / PageRead /
//! Finish / Abort, plus shared archive-read / read-only-finish steps and any unified-primary
//! union members. The function's prompt is a bare `{{ ctx.output_format }}` so BAML's existing
//! prompt renderer produces the JSON-shape catalog text once at build time. The rendered text is
//! written to [`CATALOG_SIDECAR_FILE`] inside `baml_src` and loaded by the runtime into
//! `ctx.tags['tool_schema_prelude']` — replacing the legacy `_baml_runtime.baml` source dump.
//!
//! The catalog is **stable per agent package** (its inputs are the agent manifest and IR-derived
//! tool/step types) so an LLM provider can prefix-cache it across every entry / active hop.

use std::collections::{BTreeSet, HashMap};

use baml_rt_tools::{UnifiedStepExecutorFunctionsMap, tools::ToolFunctionMetadata};
use baml_runtime::{BamlRuntime, InternalRuntimeInterface, RenderedPrompt};
use baml_types::BamlMap;
use internal_baml_core::ir::ir_hasher::IRSignature;

use super::super::ir_type_print::collect_union_type_names;
use crate::builder::error::{BamlBuilderError, Result, write_line};

/// Synthetic BAML function name carrying the agent-wide catalog union as its return type.
///
/// Lives in the generated `_baml_runtime.baml` after the second compile pass so BAML's
/// `ctx.output_format` renderer can be driven over its return type. The `__bamlrt`
/// suffix marks it as a builder-managed surface — BAML's grammar disallows leading
/// underscores on identifiers (`single_word = ASCII_ALPHA ~ ...` in the schema-ast pest
/// rule), so the prefix is impractical for namespacing.
pub const CATALOG_FUNCTION_NAME: &str = "AgentToolSchemaCatalog__bamlrt";

/// Sidecar text file holding the rendered catalog. Sits next to `_baml_runtime.baml` inside
/// `baml_src/` so it ships with the agent package and is cluster-deterministic.
pub const CATALOG_SIDECAR_FILE: &str = "_baml_tool_schema_catalog.txt";

/// IR-derived view of the catalog: the union members that must be rendered to the model.
#[derive(Debug, Default, Clone)]
pub struct CatalogPlan {
    /// Class / type-alias names forming the catalog union, sorted and deduplicated.
    pub union_type_names: Vec<String>,
}

impl CatalogPlan {
    pub fn is_empty(&self) -> bool {
        self.union_type_names.is_empty()
    }

    pub fn union_baml(&self) -> String {
        self.union_type_names.join(" | ")
    }
}

/// Compute the catalog union from compiled IR plus manifest tool metadata.
///
/// For each manifest tool we add the per-tool FSM step classes (`*OpenStep`, `*SendStep`,
/// `*FinishStep`, `*AbortStep`). We always include the shared `ArchiveSearchReadStep`,
/// `ArchivePageReadStep`, and `ReadOnlyFinishStep` when at least one tool is present. For
/// unified-primary roots we additionally collect every non-archive class named in the IR
/// return union (planner outputs, structured AskUser variants, etc.).
///
/// Names that do not resolve to a class or type alias in the compiled IR are filtered out so
/// the synthetic function compiles cleanly.
pub fn collect_catalog_types(
    ir_sig: &IRSignature,
    tool_metadata: &[ToolFunctionMetadata],
    unified_roots: &UnifiedStepExecutorFunctionsMap,
) -> CatalogPlan {
    let mut set: BTreeSet<String> = BTreeSet::new();

    for tool in tool_metadata {
        let cls = &tool.class_name;
        set.insert(format!("{cls}OpenStep"));
        set.insert(format!("{cls}SendStep"));
        set.insert(format!("{cls}FinishStep"));
        set.insert(format!("{cls}AbortStep"));
    }

    if !tool_metadata.is_empty() {
        set.insert("ArchiveSearchReadStep".to_string());
        set.insert("ArchivePageReadStep".to_string());
        set.insert("ReadOnlyFinishStep".to_string());
    }

    for fn_name in unified_roots.keys() {
        if let Some(sig) = ir_sig.functions.get(fn_name) {
            for name in collect_union_type_names(&sig.output) {
                if !name.ends_with("SessionPlan") {
                    set.insert(name);
                }
            }
        }
    }

    let valid: Vec<String> = set
        .into_iter()
        .filter(|n| ir_sig.classes.contains_key(n) || ir_sig.type_aliases.contains_key(n))
        .collect();

    CatalogPlan {
        union_type_names: valid,
    }
}

/// Pick a client name to attach to the synthetic catalog function.
///
/// The catalog function is never invoked against an LLM — it exists only so BAML's prompt
/// renderer can be driven over its return type. Any valid client suffices; we prefer the first
/// client referenced by an existing function so we inherit its env-var contract, falling back
/// to the first declared client in IR.
pub fn pick_catalog_client_name(runtime: &baml_runtime::BamlRuntime) -> Option<String> {
    use std::ops::Deref;

    let ir = runtime.ir.deref();

    for func in ir.walk_functions() {
        if let Some(config) = func.elem().configs.first() {
            let name = config.client.as_str().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    ir.walk_clients().next().map(|c| c.item.elem.name.clone())
}

/// Render the synthetic catalog function with BAML's existing prompt renderer.
///
/// Drives the same code path used for `{{ ctx.output_format }}` in real prompts. The catalog
/// function's prompt body is **only** that directive, so the rendered output is exactly the
/// JSON-shape catalog schema text without any task / objective prose. Returns the joined text
/// across rendered chat messages (or the completion body for completion-style providers).
///
/// Required environment variables for the chosen client are stubbed when the caller does not
/// supply them (the catalog render performs **no** HTTP — env vars are only consulted by
/// `get_llm_provider_impl` to satisfy `walker.required_env_vars()` parsing).
pub async fn render_catalog_prompt(
    runtime: &BamlRuntime,
    function_name: &str,
    extra_env_vars: HashMap<String, String>,
) -> Result<String> {
    let env_vars = stub_required_env_vars(runtime, extra_env_vars);
    let manager = runtime.create_ctx_manager(baml_types::BamlValue::Null, None);

    let mut ctx = manager
        .create_ctx(None, None, env_vars, vec![])
        .map_err(|e| BamlBuilderError::CatalogRender {
            message: format!("create_ctx: {e:#}"),
        })?;
    ctx.set_modular_api(true);

    let params: BamlMap<String, baml_types::BamlValue> = BamlMap::new();
    let (rendered, _scope, _allowed_metadata) = runtime
        .render_prompt(function_name, &ctx, &params, None)
        .await
        .map_err(|e| BamlBuilderError::CatalogRender {
            message: format!("render_prompt({function_name}): {e:#}"),
        })?;

    Ok(rendered_prompt_to_text(&rendered))
}

/// Concatenate every chat message body into a single block (or return the bare completion body).
///
/// `RenderedPrompt::Chat` always wraps each message into roles, but for the catalog we want a
/// flat schema text that can be injected verbatim into `ctx.tags['tool_schema_prelude']`.
fn rendered_prompt_to_text(rendered: &RenderedPrompt) -> String {
    use baml_runtime::ChatMessagePart;

    match rendered {
        RenderedPrompt::Completion(s) => s.clone(),
        RenderedPrompt::Chat(messages) => {
            let mut out = String::new();
            for msg in messages {
                for part in &msg.parts {
                    if let ChatMessagePart::Text(text) = part {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(text);
                    }
                }
            }
            out
        }
    }
}

/// Provide best-effort empty placeholders for env vars referenced by the catalog client.
///
/// `BamlRuntime::get_llm_provider_impl` resolves the LLM provider before render — for some
/// providers it bails when required env vars are unset. Render is purely template work; we
/// stub missing keys with empty strings so the renderer can complete without HTTP credentials.
fn stub_required_env_vars(
    runtime: &BamlRuntime,
    extra: HashMap<String, String>,
) -> HashMap<String, String> {
    use std::ops::Deref;

    let mut env_vars = extra;
    let ir = runtime.ir.deref();
    for client in ir.walk_clients() {
        for key in client.required_env_vars() {
            env_vars.entry(key).or_default();
        }
    }
    env_vars
}

/// Emit the synthetic catalog BAML function source. Trivial prompt body of just
/// `{{ ctx.output_format }}` so the rendered prompt is exactly the catalog schema text.
pub fn emit_catalog_function_baml(
    plan: &CatalogPlan,
    client_name: &str,
    out: &mut String,
) -> Result<()> {
    if plan.is_empty() {
        return Ok(());
    }
    write_line(out, "/// Auto-generated agent-wide tool schema catalog.")?;
    write_line(
        out,
        "/// Rendered once at build time into _baml_tool_schema_catalog.txt and loaded as ctx.tags['tool_schema_prelude'].",
    )?;
    write_line(
        out,
        "/// Never invoked against an LLM — exists only to drive BAML's ctx.output_format renderer.",
    )?;
    write_line(
        out,
        &format!(
            "function {CATALOG_FUNCTION_NAME}() -> {} {{",
            plan.union_baml()
        ),
    )?;
    write_line(out, &format!("  client {client_name}"))?;
    write_line(out, "  prompt #\"")?;
    write_line(out, "{{ ctx.output_format }}")?;
    write_line(out, "\"#")?;
    write_line(out, "}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_plan_union_baml_is_pipe_separated() {
        let plan = CatalogPlan {
            union_type_names: vec!["A".to_string(), "B".to_string(), "C".to_string()],
        };
        assert_eq!(plan.union_baml(), "A | B | C");
    }

    #[test]
    fn empty_plan_emits_nothing() {
        let plan = CatalogPlan::default();
        let mut out = String::new();
        emit_catalog_function_baml(&plan, "Stub", &mut out).expect("ok");
        assert!(out.is_empty());
    }

    #[test]
    fn emit_catalog_function_includes_all_union_members_and_client() {
        let plan = CatalogPlan {
            union_type_names: vec![
                "ArchivePageReadStep".to_string(),
                "ArchiveSearchReadStep".to_string(),
                "ReadOnlyFinishStep".to_string(),
                "SupportCalculateOpenStep".to_string(),
                "SupportCalculateSendStep".to_string(),
            ],
        };
        let mut out = String::new();
        emit_catalog_function_baml(&plan, "DefaultClient", &mut out).expect("ok");
        assert!(out.contains(&format!("function {CATALOG_FUNCTION_NAME}()")));
        assert!(out.contains("client DefaultClient"));
        assert!(out.contains("ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep"));
        assert!(out.contains("SupportCalculateOpenStep | SupportCalculateSendStep"));
        assert!(out.contains("{{ ctx.output_format }}"));
    }
}
