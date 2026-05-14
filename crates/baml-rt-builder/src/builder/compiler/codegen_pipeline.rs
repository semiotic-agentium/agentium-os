//! Named phases of the runtime codegen pipeline.
//!
//! `RuntimeTypeGenerator::generate` was previously a single ~250-line `spawn_blocking` closure
//! that mixed file I/O, three BAML compiles, IR extraction, prompt rewriting, manifest emission,
//! TypeScript declaration generation, and async catalog rendering. This module factors those
//! responsibilities into small, named phase types that hand off explicit state — invariants
//! become readable in the orchestrator's call sequence and each phase can be reasoned about in
//! isolation.
//!
//! Pipeline (sync portion runs inside `spawn_blocking`):
//! 1. [`WorkspaceReady`] — copy author baml_src into build_dir, purge legacy generated files.
//! 2. [`PreludeWritten`] — append generated tool interfaces + coordination BAML to `_baml_runtime.baml`.
//! 3. [`CompiledFirstPass`] — first `BamlRuntime::from_directory`, IR signature, session-plan
//!    parents, unified-primary roots loaded from the agent JSON manifest.
//! 4. [`PromptsNormalized`] — universal authored-prompt rewrite (skip parents/roots/catalog).
//! 5. [`SessionArtifactsEmitted`] — IR-driven polymorphic types + phase executor functions
//!    appended to the prelude; second compile if anything was added.
//! 6. [`RuntimeFinalized`] — stable IR-derived catalog sidecar payload captured for writing.
//! 7. Manifests + TypeScript declarations are written off the same final state.
//! 8. Async tail writes the rendered stable catalog sidecar when present.

use std::{collections::HashMap, fs, path::PathBuf};

use baml_rt_tools::{
    SessionPlanFunctionsMap, UnifiedStepExecutorFunctionsMap,
    external_tools::{EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalToolsLockfile, external_dirs_from_env},
    gather_coordination_fragments,
    tools::ToolFunctionMetadata,
};
use baml_runtime::BamlRuntime;
use internal_baml_core::ir::ir_hasher::IRSignature;

use super::{
    atomic_io::atomic_write,
    prompt_rewrite::{DefaultPromptRewritePolicy, rewrite_authored_prompts_in_dir},
};
use crate::builder::{
    baml_gen::{
        CATALOG_SIDECAR_FILE, GENERATED_BAML_PRELUDE_FILE, GeneratedSessionBaml,
        purge_managed_generated_baml_files, render_baml_tool_interfaces_with_mcp_root,
        render_generated_session_baml_from_ir,
    },
    baml_signature_gen::{extract_baml_signatures, session_plan_functions_map},
    error::{BamlBuilderError, Result},
    selection_hint::render_selection_hint_for_type,
    ts_gen::{load_manifest_tools, render_ts_declarations},
    types::{AgentDir, BuildDir},
};

/// Path metadata shared by every phase. Constructed once in
/// [`WorkspaceReady::materialize`]; never mutated thereafter.
pub(super) struct CodegenPaths {
    pub agent_dir: AgentDir,
    pub build_dir: BuildDir,
    pub baml_src_build: PathBuf,
    pub prelude_path: PathBuf,
}

impl CodegenPaths {
    fn new(agent_dir: AgentDir, build_dir: BuildDir) -> Self {
        let baml_src_build = build_dir.join("baml_src");
        let prelude_path = baml_src_build.join(GENERATED_BAML_PRELUDE_FILE);
        Self {
            agent_dir,
            build_dir,
            baml_src_build,
            prelude_path,
        }
    }
}

// ── Phase 1: workspace materialized ──────────────────────────────────────────

/// `build_dir/baml_src` exists and contains the agent's authored BAML files; legacy generated
/// files have been purged. Manifest-resolved tool metadata is loaded once for use by later
/// phases (coordination prelude + IR codegen).
pub(super) struct WorkspaceReady {
    pub paths: CodegenPaths,
    pub tool_names: Vec<String>,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
}

impl WorkspaceReady {
    pub(super) fn materialize(agent_dir: AgentDir, build_dir: BuildDir) -> Result<Self> {
        let paths = CodegenPaths::new(agent_dir, build_dir);
        let baml_src = paths.agent_dir.baml_src();

        if !paths.baml_src_build.exists() {
            copy_dir_all(&baml_src, &paths.baml_src_build)?;
        }
        purge_managed_generated_baml_files(&paths.baml_src_build).map_err(BamlBuilderError::Io)?;

        let tool_names = load_manifest_tools(&baml_src)?;
        let mcp_root = paths.build_dir.join("mcp");
        let mcp_root = if mcp_root.exists() {
            tracing::info!(
                mcp_root = %mcp_root.display(),
                "using packaged MCP registry snapshots during type generation"
            );
            Some(mcp_root)
        } else {
            None
        };
        let tool_metadata = if !tool_names.is_empty() {
            let catalog = baml_rt_tools::external_tools::build_builder_catalog_with_mcp_root(
                mcp_root.as_deref(),
            )?;
            baml_rt_tools::tool_catalog::resolve_manifest_tools_with_catalog(&catalog, &tool_names)?
        } else {
            Vec::new()
        };
        let unified_roots = load_unified_step_executors_authoring(&baml_src);

        Ok(Self {
            paths,
            tool_names,
            tool_metadata,
            unified_roots,
        })
    }

    pub(super) fn emit_tool_interfaces_prelude(self) -> Result<PreludeWritten> {
        let mcp_root = self.paths.build_dir.join("mcp");
        let mcp_root = mcp_root.exists().then_some(mcp_root);
        let mut generated_baml =
            render_baml_tool_interfaces_with_mcp_root(&self.tool_names, mcp_root.as_deref())?;
        if !self.tool_metadata.is_empty()
            && let Some(coord_baml) = gather_coordination_fragments(&self.tool_metadata)?
        {
            generated_baml.push_str("\n\n// ── builder: session coordination (tool crates) ──\n\n");
            generated_baml.push_str(&coord_baml);
        }

        if let Some(parent) = self.paths.prelude_path.parent() {
            fs::create_dir_all(parent).map_err(BamlBuilderError::Io)?;
        }
        atomic_write(&self.paths.prelude_path, generated_baml.as_bytes())?;

        Ok(PreludeWritten {
            paths: self.paths,
            tool_names: self.tool_names,
            tool_metadata: self.tool_metadata,
            unified_roots: self.unified_roots,
            generated_baml,
        })
    }
}

// ── Phase 2: initial prelude written ────────────────────────────────────────

pub(super) struct PreludeWritten {
    pub paths: CodegenPaths,
    pub tool_names: Vec<String>,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
    pub generated_baml: String,
}

impl PreludeWritten {
    pub(super) fn compile_first_pass(self) -> Result<CompiledFirstPass> {
        let runtime = compile_runtime(&self.paths.baml_src_build)?;
        let ir_signature = extract_baml_signatures(&runtime)?;
        let session_plan_map = session_plan_functions_map(&ir_signature);

        Ok(CompiledFirstPass {
            paths: self.paths,
            tool_names: self.tool_names,
            tool_metadata: self.tool_metadata,
            unified_roots: self.unified_roots,
            generated_baml: self.generated_baml,
            runtime,
            ir_signature,
            session_plan_map,
        })
    }
}

// ── Phase 3: first compile + IR extracted ──────────────────────────────────

pub(super) struct CompiledFirstPass {
    pub paths: CodegenPaths,
    pub tool_names: Vec<String>,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
    pub generated_baml: String,
    pub runtime: BamlRuntime,
    pub ir_signature: IRSignature,
    pub session_plan_map: SessionPlanFunctionsMap,
}

impl CompiledFirstPass {
    /// Universal Structured Prompt Compositor — rewrite hand-authored function prompts so every
    /// model-facing prompt opens with the canonical prefix. Excluded by [`DefaultPromptRewritePolicy`]:
    /// session-plan parents, unified-primary roots, and the synthetic catalog function.
    ///
    /// The first-pass runtime is preserved across this transition; it still reflects the
    /// pre-rewrite IR but session-plan parent / unified-primary parent `prompt_template` text
    /// is unchanged (those names are skipped by the policy), and only their IR is consumed by
    /// downstream phase generation. Avoids a redundant `BamlRuntime::from_directory` call.
    pub(super) fn normalize_authored_prompts(self) -> Result<PromptsNormalized> {
        let session_plan_parent_names: std::collections::HashSet<String> =
            self.session_plan_map.keys().cloned().collect();
        let unified_primary_root_names: std::collections::HashSet<String> =
            self.unified_roots.keys().cloned().collect();
        let selection_hints = authored_selection_hints(&self.ir_signature);
        let policy = DefaultPromptRewritePolicy {
            session_plan_parent_names: &session_plan_parent_names,
            unified_primary_root_names: &unified_primary_root_names,
        };
        let summary =
            rewrite_authored_prompts_in_dir(&self.paths.baml_src_build, &policy, &selection_hints)?;
        tracing::debug!(
            rewritten = summary.rewritten,
            skipped = summary.skipped,
            no_prompt = summary.no_prompt,
            "authored prompt rewriter: build pass complete"
        );

        Ok(PromptsNormalized {
            paths: self.paths,
            tool_names: self.tool_names,
            tool_metadata: self.tool_metadata,
            unified_roots: self.unified_roots,
            generated_baml: self.generated_baml,
            runtime: self.runtime,
            ir_signature: self.ir_signature,
            session_plan_map: self.session_plan_map,
        })
    }
}

fn authored_selection_hints(ir: &IRSignature) -> HashMap<String, String> {
    ir.functions
        .iter()
        .map(|(name, func_sig)| {
            (
                name.clone(),
                render_selection_hint_for_type(func_sig.output.as_ref(), ir),
            )
        })
        .collect()
}

// ── Phase 4: prompts normalized ─────────────────────────────────────────────

pub(super) struct PromptsNormalized {
    pub paths: CodegenPaths,
    pub tool_names: Vec<String>,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
    pub generated_baml: String,
    /// Pre-rewrite runtime; safe to feed to [`render_generated_session_baml_from_ir`] because
    /// session-plan parents and unified-primary roots (whose `prompt_template` is inlined) are
    /// excluded from the rewrite.
    pub runtime: BamlRuntime,
    pub ir_signature: IRSignature,
    pub session_plan_map: SessionPlanFunctionsMap,
}

impl PromptsNormalized {
    /// Generate polymorphic session unions + per-phase step executors from IR, append them to
    /// the prelude, register their session-plan entries, and recompile when anything new was
    /// emitted (so subsequent codegen and the catalog renderer see the full IR).
    pub(super) fn emit_session_artifacts(mut self) -> Result<SessionArtifactsEmitted> {
        let session_baml = render_generated_session_baml_from_ir(
            &self.runtime,
            &self.tool_metadata,
            &self.unified_roots,
        )?;

        let needs_recompile =
            !session_baml.polymorphic_types.is_empty() || !session_baml.phase_functions.is_empty();
        if !self.tool_metadata.is_empty() || !self.unified_roots.is_empty() {
            if !session_baml.polymorphic_types.is_empty() {
                self.generated_baml
                    .push_str("\n\n// ── builder: polymorphic session unions (from IR) ──\n\n");
                self.generated_baml
                    .push_str(&session_baml.polymorphic_types);
            }
            if !session_baml.phase_functions.is_empty() {
                self.generated_baml
                    .push_str("\n\n// ── builder: per-phase step executors (from IR) ──\n\n");
                self.generated_baml.push_str(&session_baml.phase_functions);
                register_phase_function_session_plans(
                    &mut self.session_plan_map,
                    &self.tool_metadata,
                );
            }
            if needs_recompile {
                atomic_write(&self.paths.prelude_path, self.generated_baml.as_bytes())?;
                let _post_session_runtime = compile_runtime(&self.paths.baml_src_build)?;
            }
        }

        Ok(SessionArtifactsEmitted {
            paths: self.paths,
            tool_names: self.tool_names,
            tool_metadata: self.tool_metadata,
            unified_roots: self.unified_roots,
            ir_signature: self.ir_signature,
            session_plan_map: self.session_plan_map,
            session_baml,
        })
    }
}

// ── Phase 5: session artifacts emitted ──────────────────────────────────────

pub(super) struct SessionArtifactsEmitted {
    pub paths: CodegenPaths,
    pub tool_names: Vec<String>,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
    pub ir_signature: IRSignature,
    pub session_plan_map: SessionPlanFunctionsMap,
    pub session_baml: GeneratedSessionBaml,
}

impl SessionArtifactsEmitted {
    /// Finalize artifacts and capture the rendered stable tool-schema sidecar text.
    pub(super) fn append_catalog_function_and_finalize(self) -> Result<RuntimeFinalized> {
        Ok(RuntimeFinalized {
            paths: self.paths,
            tool_names: self.tool_names,
            tool_metadata: self.tool_metadata,
            unified_roots: self.unified_roots,
            ir_signature: self.ir_signature,
            session_plan_map: self.session_plan_map,
            catalog_text: if self.session_baml.catalog_plan.is_empty() {
                None
            } else {
                Some(self.session_baml.catalog_plan.rendered_text)
            },
        })
    }
}

// ── Phase 6: runtime finalized; ready to emit declarations + manifests ─────

pub(super) struct RuntimeFinalized {
    pub paths: CodegenPaths,
    pub tool_names: Vec<String>,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
    pub ir_signature: IRSignature,
    pub session_plan_map: SessionPlanFunctionsMap,
    pub catalog_text: Option<String>,
}

impl RuntimeFinalized {
    pub(super) fn emit_typescript_declarations(&self) -> Result<()> {
        let declarations = render_ts_declarations(
            &self.ir_signature,
            &self.tool_names,
            &self.session_plan_map,
            &self.unified_roots,
        )?;

        let src_dts = self.paths.agent_dir.src().join("baml-runtime.d.ts");
        if let Some(parent) = src_dts.parent() {
            fs::create_dir_all(parent).map_err(BamlBuilderError::Io)?;
        }
        atomic_write(&src_dts, declarations.as_bytes())?;

        let dist_dts = self.paths.build_dir.join("dist").join("baml-runtime.d.ts");
        if let Some(parent) = dist_dts.parent() {
            fs::create_dir_all(parent).map_err(BamlBuilderError::Io)?;
        }
        atomic_write(&dist_dts, declarations.as_bytes())?;
        Ok(())
    }

    pub(super) fn emit_runtime_manifests(&self) -> Result<()> {
        if !self.session_plan_map.is_empty() {
            let manifest_path = self.paths.build_dir.join("session_plan_functions.json");
            let json = serde_json::to_string_pretty(&self.session_plan_map)
                .map_err(BamlBuilderError::Json)?;
            atomic_write(&manifest_path, json.as_bytes())?;
        }
        if !self.unified_roots.is_empty() {
            let unified_path = self
                .paths
                .build_dir
                .join("unified_step_executor_functions.json");
            let json = serde_json::to_string_pretty(&self.unified_roots)
                .map_err(BamlBuilderError::Json)?;
            atomic_write(&unified_path, json.as_bytes())?;
        }

        let tool_step_executors =
            build_tool_step_executors_map(&self.session_plan_map, &self.tool_metadata);
        if !tool_step_executors.is_empty() {
            let executors_path = self.paths.build_dir.join("tool_step_executors.json");
            let json = serde_json::to_string_pretty(&tool_step_executors)
                .map_err(BamlBuilderError::Json)?;
            atomic_write(&executors_path, json.as_bytes())?;
        }

        let lockfile = build_external_tools_lockfile(&self.tool_metadata)?;
        let lockfile_json =
            serde_json::to_string_pretty(&lockfile).map_err(BamlBuilderError::Json)?;
        let lockfile_path = self.paths.build_dir.join(EXTERNAL_TOOLS_LOCKFILE_NAME);
        atomic_write(&lockfile_path, lockfile_json.as_bytes())?;

        Ok(())
    }

    /// Inputs the async tail needs to render the catalog sidecar. `None` when the agent has no
    /// tools — the sidecar is then absent and the runtime falls back gracefully.
    pub(super) fn into_catalog_render_inputs(self) -> Option<CatalogRenderInputs> {
        self.catalog_text.map(|rendered_text| CatalogRenderInputs {
            rendered_text,
            baml_src_build: self.paths.baml_src_build,
        })
    }
}

// ── Async tail: render catalog sidecar ──────────────────────────────────────

pub(super) struct CatalogRenderInputs {
    pub rendered_text: String,
    pub baml_src_build: PathBuf,
}

impl CatalogRenderInputs {
    pub(super) async fn render_sidecar(&self) -> Result<()> {
        let sidecar_path = self.baml_src_build.join(CATALOG_SIDECAR_FILE);
        if let Some(parent) = sidecar_path.parent() {
            fs::create_dir_all(parent).map_err(BamlBuilderError::Io)?;
        }
        atomic_write(&sidecar_path, self.rendered_text.as_bytes())?;
        tracing::debug!(
            bytes = self.rendered_text.len(),
            path = %sidecar_path.display(),
            "tool schema catalog: rendered sidecar"
        );
        Ok(())
    }
}

// ── Helpers shared across phases ────────────────────────────────────────────

fn compile_runtime(baml_src_build: &std::path::Path) -> Result<BamlRuntime> {
    let env_vars: HashMap<String, String> = HashMap::new();
    let feature_flags = internal_baml_core::feature_flags::FeatureFlags::default();
    BamlRuntime::from_directory(baml_src_build, env_vars, feature_flags)
        .map_err(|e| BamlBuilderError::RuntimeLoadFailed { source: e })
}

fn load_unified_step_executors_authoring(
    baml_src: &std::path::Path,
) -> UnifiedStepExecutorFunctionsMap {
    let path = baml_src.join("unified_step_executors.json");
    if !path.is_file() {
        return UnifiedStepExecutorFunctionsMap::new();
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return UnifiedStepExecutorFunctionsMap::new();
    };
    baml_rt_tools::parse_unified_step_executors_authoring_json(&text)
}

/// After per-phase step executors are emitted, register their `__entry` / `__active__*` names
/// in the session-plan map so the runtime can resolve a tool from the invoking function name.
fn register_phase_function_session_plans(
    session_plan_map: &mut SessionPlanFunctionsMap,
    tool_metadata: &[ToolFunctionMetadata],
) {
    if tool_metadata.is_empty() {
        return;
    }
    let poly_entries: Vec<(String, Vec<baml_rt_tools::SessionPlanTypeName>)> = session_plan_map
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (func_name, plan_types) in &poly_entries {
        session_plan_map.insert(
            baml_rt_tools::SessionTypeNames::entry(func_name),
            plan_types.clone(),
        );
        for tool in tool_metadata {
            let slug = tool.name.slug();
            let tool_plan: Vec<baml_rt_tools::SessionPlanTypeName> = plan_types
                .iter()
                .filter(|pt| pt.class_name() == tool.class_name)
                .cloned()
                .collect();
            if !tool_plan.is_empty() {
                session_plan_map.insert(
                    baml_rt_tools::SessionTypeNames::active(func_name, &slug),
                    tool_plan,
                );
            }
        }
    }
}

/// For each single-tool entry in the session-plan map, expose `tool_name` → step executor name
/// so the polymorphic shim can auto-narrow after Open without a reverse index walk.
fn build_tool_step_executors_map(
    session_plan_map: &SessionPlanFunctionsMap,
    tool_metadata: &[ToolFunctionMetadata],
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
    tool_metadata: &[ToolFunctionMetadata],
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

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(BamlBuilderError::Io)?;
    for entry in fs::read_dir(src).map_err(BamlBuilderError::Io)? {
        let entry = entry.map_err(BamlBuilderError::Io)?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_all(&path, &dst_path)?;
        } else {
            fs::copy(&path, &dst_path).map_err(BamlBuilderError::Io)?;
        }
    }
    Ok(())
}
