use crate::ToolName;
use crate::tools::ToolFunctionMetadata;
use baml_rt_core::{BamlRtError, Result};
use std::collections::HashMap;

pub struct ToolMetadataProvider(pub fn() -> ToolFunctionMetadata);

inventory::collect!(ToolMetadataProvider);

pub trait ToolCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata>;
    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a>;
}

pub struct InventoryCatalog {
    tools: Vec<ToolFunctionMetadata>,
}

impl InventoryCatalog {
    pub fn new() -> Self {
        Self {
            tools: all_tool_metadata(),
        }
    }
}

impl Default for InventoryCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCatalog for InventoryCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
        self.tools.iter().find(|tool| &tool.name == name)
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
        Box::new(self.tools.iter())
    }
}

pub fn all_tool_metadata() -> Vec<ToolFunctionMetadata> {
    inventory::iter::<ToolMetadataProvider>
        .into_iter()
        .map(|provider| (provider.0)())
        .collect()
}

pub fn resolve_manifest_tools(tool_names: &[String]) -> Result<Vec<ToolFunctionMetadata>> {
    let catalog = InventoryCatalog::new();
    resolve_manifest_tools_with_catalog(&catalog, tool_names)
}

pub fn resolve_manifest_tools_with_catalog<C: ToolCatalog>(
    catalog: &C,
    tool_names: &[String],
) -> Result<Vec<ToolFunctionMetadata>> {
    let mut map: HashMap<ToolName, ToolFunctionMetadata> = HashMap::new();
    for metadata in catalog.iter() {
        map.insert(metadata.name.clone(), metadata.clone());
    }

    let mut resolved = Vec::with_capacity(tool_names.len());
    let mut missing = Vec::new();
    for name in tool_names {
        let parsed = ToolName::parse(name)?;
        match map.get(&parsed) {
            Some(metadata) => resolved.push(metadata.clone()),
            None => missing.push(name.clone()),
        }
    }

    if !missing.is_empty() {
        return Err(BamlRtError::InvalidArgument(format!(
            "Tool metadata missing for: {}",
            missing.join(", ")
        )));
    }

    Ok(resolved)
}

#[macro_export]
macro_rules! register_tool_metadata {
    ($provider:path) => {
        inventory::submit! {
            $crate::tool_catalog::ToolMetadataProvider($provider)
        }
    };
}
