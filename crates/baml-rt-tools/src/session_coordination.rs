//! Session coordination BAML provider.
//!
//! Two equivalent sources feed the builder's coordination prelude:
//!
//! 1. **Inventory providers** — internal tool crates register a render function
//!    via `inventory!`. Compiled into the binary at build time.
//! 2. **Bundle fragments** — external tool packages ship a `coordination.baml`
//!    file alongside `tool-metadata.json`. The metadata catalog reads the file
//!    into [`ToolFunctionMetadata::coordination_baml`] when the tool is loaded.
//!
//! Both paths converge in [`gather_coordination_fragments`]: the builder passes
//! resolved tool metadata, this function returns concatenated coordination BAML.
//! Origin (inventory vs bundle) is invisible to the builder.

use std::collections::HashSet;

use baml_rt_core::{BamlRtError, Result};

use crate::tools::ToolFunctionMetadata;

/// Provider for session coordination BAML. Tool crates submit these via inventory; the builder
/// collects BAML for tools named in the manifest.
pub struct SessionCoordinationProvider {
    /// Tool name (e.g. "claude/dev"). Must match manifest.
    pub tool_id: &'static str,
    /// Renders the full BAML fragment (classes, function, prompt) for this session tool.
    pub render: fn() -> Result<String>,
}

inventory::collect!(SessionCoordinationProvider);

/// Returns concatenated session coordination BAML for all tools in `tool_names` that have
/// a registered inventory provider. None if no provider matched.
///
/// Inventory-only path. Prefer [`gather_coordination_fragments`] which also reads
/// bundle-shipped fragments from external tool packages.
pub fn get_session_coordination_baml_for_tools(tool_names: &[String]) -> Result<Option<String>> {
    let set: HashSet<&str> = tool_names.iter().map(String::as_str).collect();
    if set.is_empty() {
        return Ok(None);
    }
    let mut fragments: Vec<String> = Vec::new();
    for provider in inventory::iter::<SessionCoordinationProvider> {
        if set.contains(provider.tool_id) {
            fragments.push((provider.render)()?);
        }
    }
    if fragments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(fragments.join("\n\n")))
    }
}

/// Render an inventory-registered coordination fragment for a single tool, if any.
///
/// Returns `None` when no inventory provider matches `tool_id`. This is the lookup
/// used by [`gather_coordination_fragments`] to combine inventory and bundle sources
/// per-tool.
fn render_inventory_fragment(tool_id: &str) -> Result<Option<String>> {
    for provider in inventory::iter::<SessionCoordinationProvider> {
        if provider.tool_id == tool_id {
            return Ok(Some((provider.render)()?));
        }
    }
    Ok(None)
}

/// Returns concatenated session coordination BAML for the given tool metadata.
///
/// For each tool, picks the fragment from:
///
/// - the bundle (`metadata.coordination_baml`) — set by the external metadata
///   catalog when `coordination.baml_file` is declared in `tool-metadata.json`;
/// - or the inventory provider — internal tool crates registering via `inventory!`.
///
/// Declaring both sources for the same tool is a hard error: the tool author
/// must pick one and only one source of truth.
///
/// Returns `None` when no tool has coordination BAML.
pub fn gather_coordination_fragments(tools: &[ToolFunctionMetadata]) -> Result<Option<String>> {
    let mut fragments: Vec<String> = Vec::new();
    for meta in tools {
        let tool_id = meta.name.to_string();
        let bundle = meta.coordination_baml.clone();
        let inventory = render_inventory_fragment(&tool_id)?;

        match (bundle, inventory) {
            (Some(_), Some(_)) => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "tool '{tool_id}' has coordination BAML from both an inventory provider \
                     and a bundle file; declare only one source"
                )));
            }
            (Some(b), None) => fragments.push(b),
            (None, Some(i)) => fragments.push(i),
            (None, None) => {}
        }
    }
    if fragments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(fragments.join("\n\n")))
    }
}
