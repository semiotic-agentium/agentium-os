//! Host registration: register manifest tools from the single tool-provider inventory.

use baml_rt_core::{BamlRtError, Result};

use crate::{
    access::{ToolAccessPolicy, enforce_tool_access},
    tool_catalog::{ManifestToolNames, ToolProvider},
    tools::{ToolName, ToolRegistry},
};

/// System bundle identifier (host-registered via SystemBundle, not from inventory).
const SYSTEM_BUNDLE: &str = "system";
/// Local names of system host tools.
const SYSTEM_LOCAL_NAMES: &[&str] = &["internal_a2a", "discover_agents", "discover_tools"];

/// True if this tool name is a system host tool (registered by host, not from inventory).
pub fn is_system_host_tool(name: &ToolName) -> bool {
    name.bundle().as_str() == SYSTEM_BUNDLE && SYSTEM_LOCAL_NAMES.contains(&name.local().as_str())
}

/// Register all manifest tools from the single inventory.
/// Metadata is always from type level; build() only produces the handler.
/// System tool names are skipped (host registers SystemBundle separately).
pub fn register_manifest_tools(
    registry: &ToolRegistry,
    tool_names: &ManifestToolNames,
    policy: &ToolAccessPolicy,
) -> Result<()> {
    let providers: Vec<_> = inventory::iter::<ToolProvider>.into_iter().collect();
    let by_name: std::collections::HashMap<ToolName, &ToolProvider> = providers
        .iter()
        .map(|p| {
            let meta = (p.metadata)();
            (meta.name.clone(), *p)
        })
        .collect();

    for name in tool_names.iter() {
        enforce_tool_access(&name.to_string(), policy)?;

        if is_system_host_tool(name) {
            continue;
        }

        let provider = match by_name.get(name) {
            Some(p) => p,
            None => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Unknown tool in manifest: {}",
                    name
                )));
            }
        };

        let metadata = (provider.metadata)();
        let handler = (provider.build)().map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("Tool '{}' failed to build", name),
            source: Box::new(e),
        })?;
        registry.register_dynamic(metadata, handler)?;
    }

    Ok(())
}
