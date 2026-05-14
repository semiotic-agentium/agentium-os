//! BAML text generation: one emitted prelude ([`GENERATED_BAML_PRELUDE_FILE`]), analogous to `src/baml-runtime.d.ts`.
//!
//! ## Layout
//! - [`prelude`] + [`prompt_copy`] — shared `_baml_runtime` header types (`render_generated_tools_prelude`); module docs define prompt style (Must / Use / Optional, emit vs return).
//! - [`writer`] — small line-oriented buffer ([`writer::BamlWriter`]); BAML is not JS/Rust, so we do
//! - [`tool_interfaces`] — manifest-driven tool cards + FSM step types.
//! - [`session_from_ir`] — polymorphic plans + per-phase executors from compiled IR (`phase_prompt` submodule: cue/footer/`output_format` algebra).
//!
//! [`genco`]: https://docs.rs/genco

mod escape;
mod ir_type_print;
mod prelude;
mod prompt_compositor;
mod prompt_copy;
mod prompt_normalize;
pub mod prompt_skeleton;
pub mod session_from_ir;
mod tool_interfaces;
pub mod writer;

pub(crate) use prompt_compositor::{
    PromptCompositor, ToolSessionPhaseSpec, UnifiedPrimaryPhaseSpec,
};
pub(crate) use prompt_normalize::AuthorBodySanitizer;

/// Single generated BAML bundle: shared types, tool interfaces, optional session coordination,
/// polymorphic session unions, and per-phase executors (same role as `baml-runtime.d.ts` for TS).
///
/// Leading `_` sorts before hand-written `*_prompt.baml` when tooling walks paths by ASCII name.
pub const GENERATED_BAML_PRELUDE_FILE: &str = "_baml_runtime.baml";

/// Filenames produced by this or older builder versions (remove from `baml_src` before a fresh emit).
pub fn is_managed_generated_baml_filename(file_name: &str) -> bool {
    file_name == GENERATED_BAML_PRELUDE_FILE
        || file_name == CATALOG_SIDECAR_FILE
        || matches!(
            file_name,
            "generated_tools.baml"
                | "_generated_tools.baml"
                | "00_generated_tools.baml"
                | "generated_session_coordination.baml"
                | "generated_polymorphic_sessions.baml"
                | "generated_phase_functions.baml"
        )
        || (file_name.starts_with("generated_") && file_name.ends_with(".baml"))
        || file_name.starts_with("_generated_")
        || file_name.starts_with("00_generated_")
}

/// Strip managed generated artefacts so only the newly written prelude defines shared types.
pub fn purge_managed_generated_baml_files(dir: &std::path::Path) -> std::io::Result<()> {
    use std::fs;
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_managed_generated_baml_filename(name) {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub use session_from_ir::{
    CATALOG_FUNCTION_NAME, CATALOG_SIDECAR_FILE, CatalogPlan, GeneratedSessionBaml,
    SessionPlanIrInspector, render_generated_session_baml_from_ir,
};
pub use tool_interfaces::{render_baml_tool_interfaces, render_baml_tool_interfaces_with_mcp_root};
