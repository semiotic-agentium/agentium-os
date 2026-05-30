// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool catalog: type-level metadata sources for tool discovery and resolution.
//! Supports composing multiple catalog sources (inventory, external, etc.).

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::{BamlRtError, Result};

use crate::{
    ToolName,
    tools::{ToolFunctionMetadata, ToolHandler},
};

/// Single provider type per tool: type-level metadata + build handler.
pub struct ToolProvider {
    /// Type-level metadata (name, description, schema, etc.). Always from the type.
    pub metadata: fn() -> ToolFunctionMetadata,
    /// Build runtime handler. Returns Err when tool is not compiled (e.g. feature off).
    pub build: fn() -> Result<Arc<dyn ToolHandler>>,
}

inventory::collect!(ToolProvider);

pub trait ToolCatalog: Send + Sync {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata>;
    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a>;

    /// Get config metadata for a bundle. Returns the first tool with this config_bundle.
    fn bundle_config(&self, bundle_name: &crate::BundleName) -> Option<&ToolFunctionMetadata> {
        self.iter()
            .find(|m| m.config_bundle.as_ref() == Some(bundle_name))
    }
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

/// Composable catalog that chains multiple [`ToolCatalog`] sources.
///
/// Lookups are resolved in source order (first match wins).
/// `InventoryCatalog` is typically the first source; external catalogs
/// (e.g. from lockfile/registry) can be appended later.
pub struct CompositeCatalog {
    sources: Vec<Box<dyn ToolCatalog>>,
}

impl CompositeCatalog {
    /// Create empty composite. Use [`Self::add`] to append sources.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Append a catalog source. First-added sources take priority in lookups.
    pub fn add(&mut self, source: Box<dyn ToolCatalog>) {
        self.sources.push(source);
    }
}

impl Default for CompositeCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCatalog for CompositeCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
        self.sources.iter().find_map(|s| s.by_name(name))
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
        Box::new(self.sources.iter().flat_map(|s| s.iter()))
    }
}

/// All tool metadata from the single inventory (type-level only).
pub fn all_tool_metadata() -> Vec<ToolFunctionMetadata> {
    inventory::iter::<ToolProvider>
        .into_iter()
        .map(|p| (p.metadata)())
        .collect()
}

/// Parsed list of tool names from manifest. Use at boundary instead of raw `Vec<String>`.
#[derive(Debug, Clone)]
pub struct ManifestToolNames(Vec<ToolName>);

impl ManifestToolNames {
    /// Parse manifest tool name strings into validated tool names.
    pub fn parse(names: &[String]) -> Result<Self> {
        let mut out = Vec::with_capacity(names.len());
        for s in names {
            out.push(ToolName::parse(s)?);
        }
        Ok(Self(out))
    }

    pub fn as_slice(&self) -> &[ToolName] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &ToolName> {
        self.0.iter()
    }
}

impl std::ops::Deref for ManifestToolNames {
    type Target = [ToolName];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
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
            "Tool metadata missing for: {}. Rebuild the binary with matching Cargo features so those crates link and register metadata (e.g. `cargo run -p baml-rt-builder --all-features --bin baml-agent-builder` or `--features http-tools` for support/crm, support/email, ClickUp, Notion, Slack).",
            missing.join(", ")
        )));
    }

    Ok(resolved)
}

/// Single macro per tool: submits one ToolProvider (metadata at type level + build).
#[macro_export]
macro_rules! register_tool {
    ($metadata_fn:path, $build_fn:path) => {
        inventory::submit! {
            $crate::tool_catalog::ToolProvider {
                metadata: $metadata_fn,
                build: $build_fn,
            }
        }
    };
}
