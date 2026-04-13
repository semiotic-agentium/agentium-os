//! Shared prelude for `_baml_runtime.baml`: emitted by [`super::prompt_copy::render_generated_tools_prelude`]
//! so citation and SearchRead/PageRead policy stay aligned with per-tool session interfaces.

/// Renders shared header, FSM docs, planning types, `StructuredReply` / part types, archive read inputs.
#[must_use]
pub fn generated_tools_prelude() -> String {
    super::prompt_copy::render_generated_tools_prelude()
}
