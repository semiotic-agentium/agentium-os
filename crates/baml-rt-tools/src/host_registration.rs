//! Host registration: register manifest tools from the single tool-provider inventory.

use std::collections::HashSet;

use baml_rt_core::{BamlRtError, Result};

use crate::{
    access::{ToolAccessPolicy, enforce_tool_access},
    tool_catalog::{ManifestToolNames, ToolProvider},
    tools::{ToolName, ToolRegistry},
};

/// Register all manifest tools from the single inventory.
/// Metadata is always from type level; build() only produces the handler.
/// Pre-registered host-managed tools are skipped so host tooling needn't be hard-coded in this layer.
pub fn register_manifest_tools(
    registry: &ToolRegistry,
    tool_names: &ManifestToolNames,
    policy: &ToolAccessPolicy,
) -> Result<()> {
    let pre_registered: HashSet<ToolName> = registry
        .all_metadata()
        .into_iter()
        .map(|metadata| metadata.name)
        .collect();
    let mut seen: HashSet<ToolName> = HashSet::with_capacity(tool_names.len());

    let providers: Vec<_> = inventory::iter::<ToolProvider>.into_iter().collect();
    let by_name: std::collections::HashMap<ToolName, &ToolProvider> = providers
        .iter()
        .map(|p| {
            let meta = (p.metadata)();
            (meta.name.clone(), *p)
        })
        .collect();

    for name in tool_names.iter() {
        if !seen.insert(name.clone()) {
            return Err(BamlRtError::InvalidArgument(format!(
                "Duplicate tool in manifest: {}",
                name
            )));
        }
        enforce_tool_access(&name.to_string(), policy)?;

        if pre_registered.contains(name) {
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
