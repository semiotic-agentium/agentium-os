// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Build-time catalog of external tools.
//!
//! Scans directories for `tool-metadata.json` and projects them into
//! [`ToolFunctionMetadata`] so the builder can emit BAML + TS types without
//! running the tool binary (which may not exist at build time).
//!
//! Runtime concerns — spawn, describe, policy, lifecycle — live in
//! [`super::resolver::DevModeResolver`]. This type is metadata-only.

use std::path::{Path, PathBuf};

use baml_rt_core::{BamlRtError, Result};

use super::{
    metadata::{build_tool_metadata, read_external_metadata},
    snapshot_catalog::ExternalToolSnapshotCatalog,
};
use crate::{
    ToolName,
    mcp_builder_catalog::McpSnapshotCatalog,
    tool_catalog::{CompositeCatalog, InventoryCatalog, ToolCatalog},
    tools::ToolFunctionMetadata,
};

/// Env var that points builder/runner at local external tool directories.
///
/// Value: colon-separated list of tool package directories. Each directory
/// must contain `tool-metadata.json`.
///
/// Runtime artifact requirements depend on `runtime.kind`:
/// - `process` (default): local `tool-server` binary
/// - `sandbox`: sandbox adapter image/rootfs (`tool-server` not required)
pub const BUILDER_EXTERNAL_TOOLS_ENV: &str = "BAML_EXTERNAL_TOOLS_DIR";

/// Build a [`CompositeCatalog`] for the builder: inventory first, plus an
/// external metadata source from [`BUILDER_EXTERNAL_TOOLS_ENV`] (if set).
///
/// Fails closed on any name collision between the compiled inventory and the
/// external metadata source — duplicate tool IDs across static and external
/// are not allowed (design invariant §7 #8).
pub fn build_builder_catalog() -> Result<CompositeCatalog> {
    build_builder_catalog_with_mcp_root(None)
}

pub fn build_builder_catalog_with_mcp_root(mcp_root: Option<&Path>) -> Result<CompositeCatalog> {
    let inventory = InventoryCatalog::new();

    let external = match external_dirs_from_env() {
        Some(dirs) => {
            let catalog = ExternalMetadataCatalog::from_dirs(&dirs)?;
            if catalog.is_empty() {
                None
            } else {
                Some(catalog)
            }
        }
        None => None,
    };

    let snapshot_external = ExternalToolSnapshotCatalog::from_env()?.filter(|c| !c.is_empty());

    if let Some(ext) = &external {
        // Strict collision check: any external tool name also present in
        // inventory is a hard build-time error.
        for meta in ext.iter() {
            if inventory.by_name(&meta.name).is_some() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool name collision at build time: '{}' exists in both the compiled inventory AND {}. Duplicate tool IDs across static and external are not allowed.",
                    meta.name, BUILDER_EXTERNAL_TOOLS_ENV
                )));
            }
        }
    }

    let mcp = match mcp_root {
        Some(root) => {
            let catalog = McpSnapshotCatalog::from_root(root)?;
            if catalog.is_empty() {
                None
            } else {
                Some(catalog)
            }
        }
        None => McpSnapshotCatalog::from_env()?.filter(|c| !c.is_empty()),
    };

    let mut existing_names: std::collections::HashSet<ToolName> = std::collections::HashSet::new();
    for meta in inventory.iter() {
        existing_names.insert(meta.name.clone());
    }
    if let Some(ext) = &external {
        for meta in ext.iter() {
            existing_names.insert(meta.name.clone());
        }
    }
    if let Some(ext) = &snapshot_external {
        for meta in ext.iter() {
            if !existing_names.insert(meta.name.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "External tool snapshot name collision at build time: '{}' already exists in inventory or legacy external sources.",
                    meta.name
                )));
            }
        }
    }
    if let Some(mcp) = &mcp {
        for meta in mcp.iter() {
            if !existing_names.insert(meta.name.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "MCP tool name collision at build time: '{}' already exists in inventory or external sources.",
                    meta.name
                )));
            }
        }
    }

    let mut composite = CompositeCatalog::new();
    composite.add(Box::new(inventory));
    if let Some(ext) = external {
        composite.add(Box::new(ext));
    }
    if let Some(ext) = snapshot_external {
        composite.add(Box::new(ext));
    }
    if let Some(mcp) = mcp {
        composite.add(Box::new(mcp));
    }
    Ok(composite)
}

pub fn external_dirs_from_env() -> Option<Vec<PathBuf>> {
    let raw = std::env::var(BUILDER_EXTERNAL_TOOLS_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let dirs: Vec<PathBuf> = trimmed
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    if dirs.is_empty() { None } else { Some(dirs) }
}

/// Read-only, metadata-only catalog backed by local tool package directories.
#[derive(Debug)]
pub struct ExternalMetadataCatalog {
    tools: Vec<ToolFunctionMetadata>,
}

impl ExternalMetadataCatalog {
    /// Load metadata from each directory. Each must contain `tool-metadata.json`.
    ///
    /// Unlike `DevModeResolver`, the tool binary is NOT required to exist here —
    /// this catalog is used at build time, before tools are deployed.
    pub fn from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut tools = Vec::with_capacity(dirs.len());
        let mut seen: std::collections::HashSet<ToolName> = std::collections::HashSet::new();

        for dir in dirs {
            let metadata = load_metadata(dir)?;
            if !seen.insert(metadata.name.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool '{}' loaded from {}",
                    metadata.name,
                    dir.display()
                )));
            }
            tools.push(metadata);
        }

        Ok(Self { tools })
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl ToolCatalog for ExternalMetadataCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
        self.tools.iter().find(|t| &t.name == name)
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
        Box::new(self.tools.iter())
    }
}

fn load_metadata(dir: &Path) -> Result<ToolFunctionMetadata> {
    // Builder/codegen path: read the committed source only. The host-resolved
    // `tool-metadata.lock.json` carries runtime-launch state (bind path) that
    // this layer never needs — letting it influence codegen would mean a
    // malformed local lock could break `cargo build`.
    let meta = read_external_metadata(dir)?;
    let tool_name = ToolName::parse(&meta.name)?;
    build_tool_metadata(dir, &meta, &tool_name)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{ToolName, tool_catalog::ToolCatalog, tools::ToolBackend};

    #[test]
    fn loads_metadata_without_requiring_binary() {
        let base = std::env::temp_dir().join(format!("ext-metadata-{}", Uuid::new_v4()));
        let tool_dir = base.join("tool");
        fs::create_dir_all(&tool_dir).unwrap();

        let tool_name = "support/external_build";
        let metadata = json!({
            "tool_abi_version": "1",
            "name": tool_name,
            "description": "build-time metadata test",
            "bundle": "support",
            "local_name": "external_build",
            "access_level": "read",
            "tags": ["external"],
            "invocation_mode": "single_shot",
            "schemas": {
                "input": {"type": "object", "properties": {"q": {"type": "string"}}},
                "output": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            },
            "secrets": [],
            "capabilities": {}
        });
        fs::write(
            tool_dir.join("tool-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        // Note: NO tool-server binary — build-time catalog must still succeed.
        let catalog = ExternalMetadataCatalog::from_dirs(std::slice::from_ref(&tool_dir)).unwrap();
        assert_eq!(catalog.len(), 1);

        let parsed = ToolName::parse(tool_name).unwrap();
        let meta = catalog.by_name(&parsed).expect("tool resolves by name");
        assert_eq!(meta.backend, ToolBackend::External);
        assert!(meta.tags.contains(&"external".to_string()));

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_duplicate_tool_names() {
        let base = std::env::temp_dir().join(format!("ext-dup-{}", Uuid::new_v4()));
        let tool_a = base.join("a");
        let tool_b = base.join("b");
        fs::create_dir_all(&tool_a).unwrap();
        fs::create_dir_all(&tool_b).unwrap();

        let metadata = json!({
            "tool_abi_version": "1",
            "name": "support/dup",
            "description": "d",
            "bundle": "support",
            "local_name": "dup",
            "access_level": "read",
            "invocation_mode": "single_shot",
            "schemas": {"input": {}, "output": {}},
            "secrets": [],
            "capabilities": {}
        });
        let bytes = serde_json::to_vec_pretty(&metadata).unwrap();
        fs::write(tool_a.join("tool-metadata.json"), &bytes).unwrap();
        fs::write(tool_b.join("tool-metadata.json"), &bytes).unwrap();

        let err =
            ExternalMetadataCatalog::from_dirs(&[tool_a, tool_b]).expect_err("must fail on dup");
        assert!(err.to_string().contains("duplicate external tool"));

        let _ = fs::remove_dir_all(base);
    }
}
