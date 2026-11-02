// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use baml_rt_tools::{
    SessionPlanFunctionsMap, UnifiedStepExecutorFunctionsMap, external_tool_cache,
    external_tools::{
        EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalToolInterfaceLockEntry, ExternalToolsLockfile,
        ExternalToolsetLockEntry, ToolDelivery,
    },
    gather_coordination_fragments, split_manifest_tool_entry,
    tools::{ToolFunctionMetadata, ToolName},
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
        purge_managed_generated_baml_files, render_baml_tool_interfaces_from_metadata,
        render_generated_session_baml_from_ir,
    },
    baml_signature_gen::{extract_baml_signatures, session_plan_functions_map},
    error::{BamlBuilderError, Result},
    selection_hint::render_selection_hint_for_type,
    tool_schema_graph::ToolSchemaGraph,
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
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    /// One immutable graph shared by every later codegen phase.
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
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
        let external_cache_dir = paths.build_dir.join("external-tools");
        let external_cache_root = if external_cache_dir.exists() {
            tracing::info!(
                external_cache_root = %external_cache_dir.display(),
                "using packaged external-tool registry snapshots during type generation"
            );
            Some(paths.build_dir.as_path().to_path_buf())
        } else {
            None
        };
        let static_catalog_path =
            crate::static_tool_registry::static_tool_catalog_path(paths.build_dir.as_path());
        let static_catalog = if static_catalog_path.exists() {
            tracing::info!(
                static_catalog = %static_catalog_path.display(),
                "using packaged static tool catalog during type generation"
            );
            Some(
                crate::static_tool_registry::load_static_tool_catalog_from_file(
                    &static_catalog_path,
                )
                .map_err(|err| BamlBuilderError::InvalidArgumentWithSource {
                    message: format!(
                        "failed to load static tool catalog from {}",
                        static_catalog_path.display()
                    ),
                    source: err.into(),
                })?,
            )
        } else {
            None
        };
        let tool_metadata = if !tool_names.is_empty() {
            let catalog = baml_rt_tools::external_tools::build_builder_catalog_with_static_catalog(
                mcp_root.as_deref(),
                external_cache_root.as_deref(),
                static_catalog
                    .clone()
                    .map(|catalog| Box::new(catalog) as Box<dyn baml_rt_tools::ToolCatalog>),
            )?;
            baml_rt_tools::tool_catalog::resolve_manifest_tools_with_catalog(&catalog, &tool_names)?
        } else {
            Vec::new()
        };
        let tool_schema_graph = Arc::new(ToolSchemaGraph::from_tools(&tool_metadata)?);
        let unified_roots = load_unified_step_executors_authoring(&baml_src);

        Ok(Self {
            paths,
            tool_metadata,
            tool_schema_graph,
            unified_roots,
        })
    }

    pub(super) fn emit_tool_interfaces_prelude(self) -> Result<PreludeWritten> {
        let mut generated_baml = render_baml_tool_interfaces_from_metadata(
            &self.tool_metadata,
            &self.tool_schema_graph,
        )?;
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
            tool_metadata: self.tool_metadata,
            tool_schema_graph: self.tool_schema_graph,
            unified_roots: self.unified_roots,
            generated_baml,
        })
    }
}

// ── Phase 2: initial prelude written ────────────────────────────────────────

pub(super) struct PreludeWritten {
    pub paths: CodegenPaths,
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
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
            tool_metadata: self.tool_metadata,
            tool_schema_graph: self.tool_schema_graph,
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
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
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
            tool_metadata: self.tool_metadata,
            tool_schema_graph: self.tool_schema_graph,
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
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
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
            &self.tool_schema_graph,
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
            tool_metadata: self.tool_metadata,
            tool_schema_graph: self.tool_schema_graph,
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
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
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
            tool_metadata: self.tool_metadata,
            tool_schema_graph: self.tool_schema_graph,
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
    pub tool_metadata: Vec<ToolFunctionMetadata>,
    /// The graph built in `WorkspaceReady`, retained for graph-backed TypeScript codegen.
    pub tool_schema_graph: Arc<ToolSchemaGraph>,
    pub unified_roots: UnifiedStepExecutorFunctionsMap,
    pub ir_signature: IRSignature,
    pub session_plan_map: SessionPlanFunctionsMap,
    pub catalog_text: Option<String>,
}

impl RuntimeFinalized {
    pub(super) fn emit_typescript_declarations(&self) -> Result<()> {
        let declarations = render_ts_declarations(
            &self.ir_signature,
            &self.tool_metadata,
            &self.tool_schema_graph,
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
            let sorted: BTreeMap<_, _> = self.session_plan_map.clone().into_iter().collect();
            let json = serde_json::to_string_pretty(&sorted).map_err(BamlBuilderError::Json)?;
            atomic_write(&manifest_path, json.as_bytes())?;
        }
        if !self.unified_roots.is_empty() {
            let unified_path = self
                .paths
                .build_dir
                .join("unified_step_executor_functions.json");
            let sorted: BTreeMap<_, _> = self.unified_roots.clone().into_iter().collect();
            let json = serde_json::to_string_pretty(&sorted).map_err(BamlBuilderError::Json)?;
            atomic_write(&unified_path, json.as_bytes())?;
        }

        let tool_step_executors =
            build_tool_step_executors_map(&self.session_plan_map, &self.tool_metadata);
        if !tool_step_executors.is_empty() {
            let executors_path = self.paths.build_dir.join("tool_step_executors.json");
            let sorted: BTreeMap<_, _> = tool_step_executors.into_iter().collect();
            let json = serde_json::to_string_pretty(&sorted).map_err(BamlBuilderError::Json)?;
            atomic_write(&executors_path, json.as_bytes())?;
        }

        let lockfile = build_external_tools_lockfile(&self.tool_metadata, &self.paths)?;
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

/// After per-phase step executors are emitted, register their `__entry` / `__active__*` /
/// `__consume__*` names in the session-plan map so the runtime can resolve a tool from the
/// invoking function name on every phase.
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
                    tool_plan.clone(),
                );
                session_plan_map.insert(
                    baml_rt_tools::SessionTypeNames::consume(func_name, &slug),
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
    let mut function_plans: Vec<_> = session_plan_map.iter().collect();
    function_plans.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));

    let mut map = HashMap::new();
    for (func_name, plan_types) in function_plans {
        if plan_types.len() == 1 {
            let class_name = plan_types[0].class_name();
            if let Some(tool_name) = class_to_tool.get(class_name) {
                // Several generated phases can share a tool. The root function sorts before its
                // generated variants and is the stable executor this reverse lookup exposes.
                map.entry(tool_name.clone())
                    .or_insert_with(|| func_name.clone());
            }
        }
    }
    map
}

/// One toolset's version constraint from the agent manifest (R1 §R1.6).
#[derive(Debug)]
struct ToolsetRequirement {
    req: semver::VersionReq,
    /// The requirement exactly as authored, recorded into the lockfile.
    raw: String,
}

/// Parse per-toolset `@<semver-req>` requirements out of the agent manifest's
/// `tools` list. All entries that name the same toolset must carry the same
/// requirement (or none); a conflict is a manifest error. Entries without a
/// requirement do not constrain the toolset (`*`).
fn parse_toolset_requirements(
    baml_src: &Path,
) -> Result<std::collections::BTreeMap<String, ToolsetRequirement>> {
    let entries = load_manifest_tools(baml_src)?;
    let mut out = std::collections::BTreeMap::<String, ToolsetRequirement>::new();
    for entry in &entries {
        let (name, requirement) = split_manifest_tool_entry(entry);
        let Some(raw) = requirement else { continue };
        let tool_name = ToolName::parse(name).map_err(|err| {
            BamlBuilderError::InvalidArgument(format!(
                "invalid manifest tool entry '{entry}': {err}"
            ))
        })?;
        let bundle = tool_name.bundle().as_str().to_string();
        let req = semver::VersionReq::parse(raw).map_err(|err| {
            BamlBuilderError::InvalidArgument(format!(
                "invalid version requirement '{raw}' for tool '{name}': {err}"
            ))
        })?;
        if let Some(existing) = out.get(&bundle) {
            if existing.raw != raw {
                return Err(BamlBuilderError::InvalidArgument(format!(
                    "toolset '{bundle}' is listed with conflicting version requirements \
                     ('{}' and '{raw}'); all entries for one toolset must agree",
                    existing.raw
                )));
            }
        } else {
            out.insert(
                bundle,
                ToolsetRequirement {
                    req,
                    raw: raw.to_string(),
                },
            );
        }
    }
    Ok(out)
}

fn build_external_tools_lockfile(
    tool_metadata: &[ToolFunctionMetadata],
    paths: &CodegenPaths,
) -> Result<ExternalToolsLockfile> {
    let external_tool_names: HashSet<String> = tool_metadata
        .iter()
        .filter(|meta| matches!(meta.backend, baml_rt_tools::ToolBackend::External))
        .map(|meta| meta.name.to_string())
        .collect();
    if external_tool_names.is_empty() {
        return Ok(ExternalToolsLockfile::empty());
    }

    let requirements = parse_toolset_requirements(&paths.agent_dir.baml_src())?;
    let cached_snapshots = external_tool_cache::read_approved_snapshots(paths.build_dir.as_path())
        .map_err(BamlBuilderError::from)?;
    resolve_lockfile_toolsets(&external_tool_names, &requirements, &cached_snapshots)
}

/// Pin each used toolset to the newest approved version satisfying its manifest
/// requirement (R1 §R1.6). Kept separate from manifest/filesystem reading so the
/// version-selection rules can be tested directly with constructed snapshots.
fn resolve_lockfile_toolsets(
    external_tool_names: &HashSet<String>,
    requirements: &std::collections::BTreeMap<String, ToolsetRequirement>,
    cached_snapshots: &[baml_rt_tools::external_tools::ExternalToolSnapshot],
) -> Result<ExternalToolsLockfile> {
    // Group the approved snapshots a used tool could resolve to, by toolset,
    // parsing each candidate's version so the requirement can be applied.
    struct Candidate {
        local: String,
        version: semver::Version,
        descriptor_digest: String,
        tool_digest: String,
        tier: &'static str,
    }
    let mut by_bundle = std::collections::BTreeMap::<String, Vec<Candidate>>::new();

    for snapshot in cached_snapshots {
        if !external_tool_names.contains(&snapshot.tool.name) {
            continue;
        }
        let Some(v2) = snapshot.v2.as_ref() else {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "manifest uses external tool {}, but its approved snapshot is not v2",
                snapshot.tool.name
            )));
        };
        let (bundle, local) = v2.descriptor.name.split_once('/').ok_or_else(|| {
            BamlBuilderError::InvalidArgument(format!(
                "external tool '{}' is not named bundle/local",
                v2.descriptor.name
            ))
        })?;
        let version = semver::Version::parse(&v2.descriptor.version).map_err(|err| {
            BamlBuilderError::InvalidArgument(format!(
                "approved snapshot for '{}' has an invalid version '{}': {err}",
                v2.descriptor.name, v2.descriptor.version
            ))
        })?;
        let (tool_digest, tier) = match &v2.delivery {
            ToolDelivery::Bundled { tool_digest, .. } => (tool_digest.to_string(), "sandbox"),
            ToolDelivery::Reference { endpoint, .. } => (
                baml_rt_tools::mcp_snapshot::canonical_digest(endpoint).to_string(),
                "service",
            ),
        };
        by_bundle
            .entry(bundle.to_string())
            .or_default()
            .push(Candidate {
                local: local.to_string(),
                version,
                descriptor_digest: v2.descriptor_digest.to_string(),
                tool_digest,
                tier,
            });
    }

    let mut toolsets = Vec::with_capacity(by_bundle.len());
    // Tracks which used tools a pinned toolset version actually covers, so a tool
    // that exists only at a non-selected version is reported rather than dropped.
    let mut covered_names = HashSet::new();
    for (bundle, candidates) in by_bundle {
        let (req, raw) = match requirements.get(&bundle) {
            Some(requirement) => (requirement.req.clone(), requirement.raw.clone()),
            None => (semver::VersionReq::STAR, "*".to_string()),
        };
        // Pin the newest approved version that satisfies the requirement.
        let Some(target) = candidates
            .iter()
            .map(|candidate| &candidate.version)
            .filter(|version| req.matches(version))
            .max()
            .cloned()
        else {
            let mut available: Vec<String> =
                candidates.iter().map(|c| c.version.to_string()).collect();
            available.sort();
            available.dedup();
            return Err(BamlBuilderError::InvalidArgument(format!(
                "toolset '{bundle}' has no approved version satisfying '{raw}' \
                 (available: {})",
                available.join(", ")
            )));
        };

        let selected: Vec<&Candidate> = candidates
            .iter()
            .filter(|candidate| candidate.version == target)
            .collect();
        let tool_digest = selected[0].tool_digest.clone();
        let tier = selected[0].tier;
        if selected.iter().any(|c| c.tool_digest != tool_digest) {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "external toolset '{bundle}' resolved to more than one tool digest at version {target}"
            )));
        }
        let mut tools: Vec<ExternalToolInterfaceLockEntry> = selected
            .iter()
            .map(|candidate| {
                covered_names.insert(format!("{bundle}/{}", candidate.local));
                ExternalToolInterfaceLockEntry {
                    name: candidate.local.clone(),
                    descriptor_digest: candidate.descriptor_digest.clone(),
                }
            })
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        toolsets.push(ExternalToolsetLockEntry {
            name: bundle,
            version: target.to_string(),
            requirement: raw,
            tier: tier.to_string(),
            tool_digest,
            tools,
        });
    }
    toolsets.sort_by(|a, b| a.name.cmp(&b.name));

    // Every used tool must be covered by a pinned toolset version. A tool missing
    // here either has no approved snapshot at all, or exists only at a version the
    // toolset's requirement excluded.
    let mut missing: Vec<String> = external_tool_names
        .difference(&covered_names)
        .cloned()
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(BamlBuilderError::InvalidArgument(format!(
            "manifest uses external tool {}, but no approved registry snapshot satisfies it",
            missing[0]
        )));
    }

    Ok(ExternalToolsLockfile {
        version: "2".to_string(),
        toolsets,
    })
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

#[cfg(test)]
mod external_tools_lockfile_tests {
    use std::collections::{BTreeMap, HashSet};

    use baml_rt_tools::{
        external_tools::{CapabilityRequest, ExternalToolSnapshot, default_node_runtime_image},
        mcp_snapshot::Digest,
    };

    use super::{ToolsetRequirement, parse_toolset_requirements, resolve_lockfile_toolsets};

    fn v2_snapshot(name: &str, version: &str, seed: u8) -> ExternalToolSnapshot {
        let mut descriptor = test_support::external_tools_v2::descriptor(name);
        descriptor.version = version.to_string();
        ExternalToolSnapshot::from_v2_bundle(
            descriptor,
            CapabilityRequest::default(),
            Digest::from_bytes([seed; 32]),
            Digest::from_bytes([seed.wrapping_add(1); 32]),
            default_node_runtime_image(),
            "operator",
            "2026-11-02T00:00:00Z",
        )
        .unwrap()
    }

    fn requirement(raw: &str) -> ToolsetRequirement {
        ToolsetRequirement {
            req: semver::VersionReq::parse(raw).unwrap(),
            raw: raw.to_string(),
        }
    }

    fn names(entries: &[&str]) -> HashSet<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pins_newest_version_satisfying_requirement() {
        let snapshots = vec![
            v2_snapshot("support/clickup", "1.0.0", 1),
            v2_snapshot("support/clickup", "1.3.0", 2),
            v2_snapshot("support/clickup", "2.0.0", 3),
        ];
        let mut requirements = BTreeMap::new();
        requirements.insert("support".to_string(), requirement("^1.2"));

        let lock =
            resolve_lockfile_toolsets(&names(&["support/clickup"]), &requirements, &snapshots)
                .unwrap();
        assert_eq!(lock.toolsets.len(), 1);
        let set = &lock.toolsets[0];
        assert_eq!(set.name, "support");
        assert_eq!(set.version, "1.3.0", "newest satisfying ^1.2, not 2.0.0");
        assert_eq!(set.requirement, "^1.2");
        assert_eq!(set.tools.len(), 1);
        assert_eq!(set.tools[0].name, "clickup");
    }

    #[test]
    fn defaults_to_newest_when_no_requirement() {
        let snapshots = vec![
            v2_snapshot("support/clickup", "1.0.0", 1),
            v2_snapshot("support/clickup", "2.0.0", 2),
        ];
        let lock =
            resolve_lockfile_toolsets(&names(&["support/clickup"]), &BTreeMap::new(), &snapshots)
                .unwrap();
        assert_eq!(lock.toolsets[0].version, "2.0.0");
        assert_eq!(lock.toolsets[0].requirement, "*");
    }

    #[test]
    fn errors_when_no_version_satisfies_requirement() {
        let snapshots = vec![
            v2_snapshot("support/clickup", "1.0.0", 1),
            v2_snapshot("support/clickup", "2.0.0", 2),
        ];
        let mut requirements = BTreeMap::new();
        requirements.insert("support".to_string(), requirement("^3"));

        let err =
            resolve_lockfile_toolsets(&names(&["support/clickup"]), &requirements, &snapshots)
                .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no approved version satisfying"), "{msg}");
        assert!(msg.contains("^3"), "{msg}");
    }

    #[test]
    fn errors_when_used_tool_has_no_snapshot() {
        let err = resolve_lockfile_toolsets(&names(&["support/clickup"]), &BTreeMap::new(), &[])
            .unwrap_err();
        assert!(
            err.to_string().contains("no approved registry snapshot"),
            "{err}"
        );
    }

    fn write_manifest(tools: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let baml_src = dir.path().join("baml_src");
        std::fs::create_dir_all(&baml_src).unwrap();
        let tools_json = serde_json::to_string(tools).unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            format!(r#"{{"version":"1.0.0","name":"a","tools":{tools_json}}}"#),
        )
        .unwrap();
        dir
    }

    #[test]
    fn parse_toolset_requirements_reads_ranges_and_ignores_bare() {
        let dir = write_manifest(&["support/clickup@^1.2", "system/internal_a2a"]);
        let reqs = parse_toolset_requirements(&dir.path().join("baml_src")).unwrap();
        assert_eq!(reqs.get("support").unwrap().raw, "^1.2");
        assert!(
            reqs.get("system").is_none(),
            "bare entries do not constrain"
        );
    }

    #[test]
    fn parse_toolset_requirements_rejects_conflicting_requirements() {
        let dir = write_manifest(&["support/clickup@^1.2", "support/other@^2"]);
        let err = parse_toolset_requirements(&dir.path().join("baml_src")).unwrap_err();
        assert!(
            err.to_string().contains("conflicting version requirements"),
            "{err}"
        );
    }
}
