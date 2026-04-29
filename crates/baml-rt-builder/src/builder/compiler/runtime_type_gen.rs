//! Generate `baml-runtime.d.ts`, generated BAML fragments, and session-plan metadata.

use std::{collections::HashMap, fs, path::Path};

use baml_rt_tools::{
    external_tools::{EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalToolsLockfile, external_dirs_from_env},
    gather_coordination_fragments,
};
use baml_runtime::BamlRuntime;
use tokio::task;

use super::atomic_io::atomic_write;
use crate::builder::{
    baml_gen::{
        GENERATED_BAML_PRELUDE_FILE, purge_managed_generated_baml_files,
        render_baml_tool_interfaces, render_generated_session_baml_from_ir,
    },
    baml_signature_gen::{extract_baml_signatures, session_plan_functions_map},
    error::{BamlBuilderError, Result},
    traits::TypeGenerator,
    ts_gen::{load_manifest_tools, render_ts_declarations},
    types::{AgentDir, BuildDir},
};

/// Type generator for runtime declarations.
///
/// Writes `baml-runtime.d.ts` into the agent's `src/` directory so that both
/// `tsc` and IDEs can resolve the types without any temp-dir indirection.
pub struct RuntimeTypeGenerator;

impl RuntimeTypeGenerator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuntimeTypeGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TypeGenerator for RuntimeTypeGenerator {
    async fn generate(&self, agent_dir: &AgentDir, build_dir: &BuildDir) -> Result<()> {
        let agent_dir = agent_dir.clone();
        let build_dir = build_dir.clone();

        task::spawn_blocking(move || {
            let baml_src = agent_dir.baml_src();

            // Generate BAML tool interfaces into build_dir/baml_src so the packaged baml_src
            // contains them (packager adds build_dir/baml_src to the tar).
            // When build_dir/baml_src does not exist (e.g. bootstrap), copy source baml_src so the runtime can load it.
            let baml_src_build = build_dir.join("baml_src");
            if !baml_src_build.exists() {
                copy_dir_all_impl(&baml_src, &baml_src_build)?;
            }
            // Agent trees may still contain legacy `generated_tools.baml` etc.; remove so the new
            // single prelude is the only copy (avoids duplicate `StandardAgentPlanStep` / FSM types).
            purge_managed_generated_baml_files(&baml_src_build).map_err(BamlBuilderError::Io)?;

            let tool_names = load_manifest_tools(&baml_src)?;

            // Resolve tool metadata once — used for the coordination prelude,
            // polymorphic type generation, and per-phase function generation.
            // Coordination BAML is sourced from `metadata.coordination_baml`,
            // which is populated by the catalog from inventory providers
            // (internal tools) or `coordination.baml_file` (external bundles).
            let tool_metadata = if !tool_names.is_empty() {
                let catalog = baml_rt_tools::external_tools::build_builder_catalog()?;
                baml_rt_tools::tool_catalog::resolve_manifest_tools_with_catalog(
                    &catalog,
                    &tool_names,
                )?
            } else {
                Vec::new()
            };

            // Single `_baml_runtime.baml` prelude (mirrors `baml-runtime.d.ts`): tools + coordination + IR sections.
            let mut generated_baml = render_baml_tool_interfaces(&tool_names)?;
            if !tool_metadata.is_empty()
                && let Some(coord_baml) = gather_coordination_fragments(&tool_metadata)?
            {
                generated_baml
                    .push_str("\n\n// ── builder: session coordination (tool crates) ──\n\n");
                generated_baml.push_str(&coord_baml);
            }

            let prelude_path = baml_src_build.join(GENERATED_BAML_PRELUDE_FILE);
            if let Some(parent) = prelude_path.parent() {
                fs::create_dir_all(parent).map_err(BamlBuilderError::Io)?;
            }
            atomic_write(&prelude_path, generated_baml.as_bytes())?;

            // First compile: user BAML + generated tool interfaces.
            // Polymorphic session types are generated from the IR *after* this compile,
            // so we do not need a pre-compile source scan.
            let env_vars: HashMap<String, String> = HashMap::new();
            let feature_flags = internal_baml_core::feature_flags::FeatureFlags::default();

            let runtime = BamlRuntime::from_directory(&baml_src_build, env_vars, feature_flags)
                .map_err(|e| BamlBuilderError::RuntimeLoadFailed { source: e })?;

            let ir_signature = extract_baml_signatures(&runtime)?;
            let session_plan_map = session_plan_functions_map(&ir_signature);

            // Generate polymorphic session types AND per-phase step executor functions
            // from the compiled IR — single pass, no source text parsing.
            let mut session_plan_map = session_plan_map;
            if !tool_metadata.is_empty() {
                let session_baml = render_generated_session_baml_from_ir(&runtime, &tool_metadata)?;

                if !session_baml.polymorphic_types.is_empty() {
                    generated_baml
                        .push_str("\n\n// ── builder: polymorphic session unions (from IR) ──\n\n");
                    generated_baml.push_str(&session_baml.polymorphic_types);
                }
                if !session_baml.phase_functions.is_empty() {
                    generated_baml
                        .push_str("\n\n// ── builder: per-phase step executors (from IR) ──\n\n");
                    generated_baml.push_str(&session_baml.phase_functions);

                    // Register phase functions for all session functions (single- and multi-tool).
                    let poly_entries: Vec<(String, Vec<baml_rt_tools::SessionPlanTypeName>)> =
                        session_plan_map
                            .iter()
                            .filter(|(_, v)| !v.is_empty())
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                    for (func_name, plan_types) in &poly_entries {
                        session_plan_map.insert(
                            baml_rt_tools::SessionTypeNames::select(func_name),
                            plan_types.clone(),
                        );
                        for tool in &tool_metadata {
                            let slug = tool.name.slug();
                            let tool_plan: Vec<baml_rt_tools::SessionPlanTypeName> = plan_types
                                .iter()
                                .filter(|pt| pt.class_name() == tool.class_name)
                                .cloned()
                                .collect();
                            if !tool_plan.is_empty() {
                                session_plan_map.insert(
                                    baml_rt_tools::SessionTypeNames::act(func_name, &slug),
                                    tool_plan.clone(),
                                );
                                // No __consume__ phase: Send blocks until Done.
                                session_plan_map.insert(
                                    baml_rt_tools::SessionTypeNames::r#continue(func_name, &slug),
                                    tool_plan,
                                );
                            }
                        }
                    }
                }

                // Second compile to include polymorphic unions and phase functions in the prelude.
                if !session_baml.polymorphic_types.is_empty()
                    || !session_baml.phase_functions.is_empty()
                {
                    atomic_write(&prelude_path, generated_baml.as_bytes())?;
                    let env_vars2: HashMap<String, String> = HashMap::new();
                    let feature_flags2 = internal_baml_core::feature_flags::FeatureFlags::default();
                    let _runtime2 =
                        BamlRuntime::from_directory(&baml_src_build, env_vars2, feature_flags2)
                            .map_err(|e| BamlBuilderError::RuntimeLoadFailed { source: e })?;
                }
            }
            let declarations =
                render_ts_declarations(&ir_signature, &tool_names, &session_plan_map)?;

            // Write baml-runtime.d.ts into agent's src/ so tsc resolves it directly.
            let src_dts = agent_dir.src().join("baml-runtime.d.ts");
            if let Some(parent) = src_dts.parent() {
                fs::create_dir_all(parent)?;
            }
            atomic_write(&src_dts, declarations.as_bytes())?;

            // Also write to build_dir/dist/ for packaging (the .d.ts is included in the tar).
            let dist_dts = build_dir.join("dist").join("baml-runtime.d.ts");
            if let Some(parent) = dist_dts.parent() {
                fs::create_dir_all(parent)?;
            }
            atomic_write(&dist_dts, declarations.as_bytes())?;

            // Emit session-plan function map so the runtime can resolve tool from the invoking
            // function name (no reliance on __type in prompt output).
            // Values are always arrays of plan type names (length 1 = single-tool, >1 = polymorphic).
            if !session_plan_map.is_empty() {
                let manifest_path = build_dir.join("session_plan_functions.json");
                let json = serde_json::to_string_pretty(&session_plan_map)
                    .map_err(BamlBuilderError::Json)?;
                atomic_write(&manifest_path, json.as_bytes())?;
            }

            // Emit tool-to-step-executor mapping for polymorphic shim auto-narrowing.
            // Direct tool_name → single-tool step executor function name.
            let tool_step_executors =
                build_tool_step_executors_map(&session_plan_map, &tool_metadata);
            if !tool_step_executors.is_empty() {
                let executors_path = build_dir.join("tool_step_executors.json");
                let json = serde_json::to_string_pretty(&tool_step_executors)
                    .map_err(BamlBuilderError::Json)?;
                atomic_write(&executors_path, json.as_bytes())?;
            }

            // Always emit external_tools.lock.json at package root (empty if no externals).
            let lockfile = build_external_tools_lockfile(&tool_metadata)?;
            let lockfile_json =
                serde_json::to_string_pretty(&lockfile).map_err(BamlBuilderError::Json)?;
            let lockfile_path = build_dir.join(EXTERNAL_TOOLS_LOCKFILE_NAME);
            atomic_write(&lockfile_path, lockfile_json.as_bytes())?;

            Ok(())
        })
        .await
        .map_err(|e| BamlBuilderError::BlockingTaskJoin { source: e })?
    }
}

/// Build a mapping from tool_name to single-tool step executor function name.
///
/// For each single-tool entry (Vec length 1) in the session plan map, maps the
/// tool's qualified name (e.g. "support/calculate") to the BAML function name
/// (e.g. "ChooseCalcAction"). Used by the shim to auto-narrow after a polymorphic
/// Open selects a tool — one direct lookup, no reverse index.
fn build_tool_step_executors_map(
    session_plan_map: &baml_rt_tools::SessionPlanFunctionsMap,
    tool_metadata: &[baml_rt_tools::tools::ToolFunctionMetadata],
) -> HashMap<String, String> {
    let class_to_tool: HashMap<&str, String> = tool_metadata
        .iter()
        .map(|m| (m.class_name.as_str(), m.name.to_string()))
        .collect();

    let mut map = HashMap::new();
    for (func_name, plan_types) in session_plan_map {
        if plan_types.len() == 1 {
            let class_name = plan_types[0].class_name();
            if let Some(tool_name) = class_to_tool.get(class_name) {
                map.insert(tool_name.clone(), func_name.clone());
            }
        }
    }
    map
}

fn build_external_tools_lockfile(
    tool_metadata: &[baml_rt_tools::tools::ToolFunctionMetadata],
) -> Result<ExternalToolsLockfile> {
    let external_tool_count = tool_metadata
        .iter()
        .filter(|meta| matches!(meta.backend, baml_rt_tools::ToolBackend::External))
        .count();

    if external_tool_count == 0 {
        return Ok(ExternalToolsLockfile::empty());
    }

    let dirs = external_dirs_from_env().ok_or_else(|| {
        BamlBuilderError::InvalidArgument(
            "manifest uses external tools, but BAML_EXTERNAL_TOOLS_DIR is not set; builder must hash local tool artifacts to produce external_tools.lock.json"
                .to_string(),
        )
    })?;

    ExternalToolsLockfile::from_tool_dirs(&dirs).map_err(BamlBuilderError::from)
}

fn copy_dir_all_impl(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(BamlBuilderError::Io)?;
    for entry in fs::read_dir(src).map_err(BamlBuilderError::Io)? {
        let entry = entry.map_err(BamlBuilderError::Io)?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all_impl(&path, &dst_path)?;
        } else {
            fs::copy(&path, &dst_path).map_err(BamlBuilderError::Io)?;
        }
    }
    Ok(())
}
