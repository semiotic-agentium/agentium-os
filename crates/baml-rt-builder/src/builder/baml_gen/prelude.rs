//! Static BAML fragments shipped as real `.baml` files under [`templates/`](templates)
//! so editors and reviewers see syntax-highlighted source instead of Rust string soup.

/// Shared header, FSM docs, planning types, `StructuredReply` / part types, `ArchiveReadInput`.
pub const GENERATED_TOOLS_PRELUDE: &str = include_str!("templates/generated_tools_prelude.baml");
