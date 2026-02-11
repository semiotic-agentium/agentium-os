use crate::ToolName;
use crate::tool_catalog::{InventoryCatalog, ToolCatalog};
use crate::tools::ToolAccess;
use baml_rt_core::{BamlRtError, Result};
use std::collections::HashSet;
use tracing::warn;

pub fn parse_access_allowlist() -> Option<HashSet<ToolAccess>> {
    let raw = std::env::var("BAML_TOOL_ACCESS_ALLOWLIST").ok()?;
    let mut set = HashSet::new();
    for token in raw.split(',') {
        let value = token.trim().to_lowercase();
        let access = match value.as_str() {
            "read" => ToolAccess::Read,
            "write" => ToolAccess::Write,
            "delete" => ToolAccess::Delete,
            "" => continue,
            other => {
                warn!(
                    value = other,
                    "Unknown access in BAML_TOOL_ACCESS_ALLOWLIST"
                );
                continue;
            }
        };
        set.insert(access);
    }
    if set.is_empty() { None } else { Some(set) }
}

pub fn enforce_tool_access(tool_name: &str, allowlist: &Option<HashSet<ToolAccess>>) -> Result<()> {
    let Some(allowlist) = allowlist else {
        return Ok(());
    };

    let catalog = InventoryCatalog::new();
    if let Some(metadata) = catalog.by_name(&ToolName::parse(tool_name)?) {
        if let Some(access) = metadata.access {
            if !allowlist.contains(&access) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool '{tool_name}' access '{access}' is not allowed by BAML_TOOL_ACCESS_ALLOWLIST"
                )));
            }
        } else {
            warn!(
                tool = tool_name,
                "Tool has no declared access; allowing due to BAML_TOOL_ACCESS_ALLOWLIST"
            );
        }
    }
    Ok(())
}
