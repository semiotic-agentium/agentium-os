//! Session coordination BAML provider: tool crates register BAML (classes + prompt) for session tools.
//! The builder discovers providers by manifest tool names and concatenates their BAML.

use std::collections::HashSet;

use baml_rt_core::Result;

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
/// a registered provider. None if no provider matched.
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
