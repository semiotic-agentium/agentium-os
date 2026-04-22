//! Host registration: register manifest tools from the single tool-provider inventory.
//! Supports an optional external fallback resolver for tools not found in inventory.

use std::{collections::HashSet, sync::Arc};

use baml_rt_core::{BamlRtError, Result};

use crate::{
    access::{ToolAccessPolicy, enforce_tool_access},
    tool_catalog::{ManifestToolNames, ToolProvider},
    tools::{ToolFunctionMetadata, ToolHandler, ToolName, ToolRegistry},
};

/// Extension point for resolving tools that are not compiled into the runner.
///
/// When `register_manifest_tools` cannot find a tool in the inventory, it
/// consults the fallback resolver (if provided). Returning `None` means the
/// resolver does not know about the tool either, which is a hard error.
pub trait ExternalToolResolver: Send + Sync {
    fn resolve(
        &self,
        name: &ToolName,
    ) -> Result<Option<(ToolFunctionMetadata, Arc<dyn ToolHandler>)>>;
}

/// Register all manifest tools from the single inventory.
/// Metadata is always from type level; build() only produces the handler.
/// Pre-registered host-managed tools are skipped so host tooling needn't be hard-coded in this layer.
///
/// When `fallback` is `None`, behavior is identical to the original implementation.
pub fn register_manifest_tools(
    registry: &ToolRegistry,
    tool_names: &ManifestToolNames,
    policy: &ToolAccessPolicy,
) -> Result<()> {
    register_manifest_tools_with_fallback(registry, tool_names, policy, None)
}

/// Same as [`register_manifest_tools`] but accepts an optional external fallback resolver.
///
/// If a manifest tool is not found in the inventory **and** a fallback is provided,
/// the fallback is consulted before returning an error.
pub fn register_manifest_tools_with_fallback(
    registry: &ToolRegistry,
    tool_names: &ManifestToolNames,
    policy: &ToolAccessPolicy,
    fallback: Option<&dyn ExternalToolResolver>,
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
            // Host-bundle tool already registered. Still refuse to let an
            // external resolver shadow it — fail closed per design invariant.
            if let Some(resolver) = fallback
                && resolver.resolve(name)?.is_some()
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool name collision: '{}' is registered by a host bundle AND declared by the external resolver. Rename one.",
                    name
                )));
            }
            continue;
        }

        let inventory_hit = by_name.get(name);
        let external_hit = match fallback {
            Some(resolver) => resolver.resolve(name)?,
            None => None,
        };

        match (inventory_hit, external_hit) {
            (Some(_), Some(_)) => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool name collision: '{}' exists in both the compiled inventory AND the external resolver. Duplicate tool IDs across static and external are not allowed.",
                    name
                )));
            }
            (Some(provider), None) => {
                let metadata = (provider.metadata)();
                let handler =
                    (provider.build)().map_err(|e| BamlRtError::InvalidArgumentWithSource {
                        message: format!("Tool '{}' failed to build", name),
                        source: Box::new(e),
                    })?;
                registry.register_dynamic(metadata, handler)?;
            }
            (None, Some((metadata, handler))) => {
                registry.register_dynamic(metadata, handler)?;
            }
            (None, None) => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Unknown tool in manifest: {}",
                    name
                )));
            }
        }
    }

    Ok(())
}
