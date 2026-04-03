//! Shared prelude for `_baml_runtime.baml`: emitted by [`super::prompt_copy::render_generated_tools_prelude`]
//! so citation and Read/grep policy stay aligned with per-tool session interfaces.

/// Renders shared header, FSM docs, planning types, `StructuredReply` / part types, `ArchiveReadInput`.
#[must_use]
pub fn generated_tools_prelude() -> String {
    super::prompt_copy::render_generated_tools_prelude()
}
