// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Build-time catalog composition for tools.
//!
//! External tools enter the builder through approved snapshots only. Authored
//! source dirs (`BAML_EXTERNAL_TOOLS_DIR`) are discovered by the runner into
//! snapshots before they become build/runtime metadata.

use std::path::{Path, PathBuf};

use baml_rt_core::{BamlRtError, Result};

use super::snapshot_catalog::ExternalToolSnapshotCatalog;
use crate::{
    ToolName,
    mcp_builder_catalog::McpSnapshotCatalog,
    tool_catalog::{CompositeCatalog, InventoryCatalog, ToolCatalog},
};

/// Env var that points runner at local external tool source directories.
///
/// Value: colon-separated list of tool package directories. Each directory
/// must contain `tool-manifest.json`. Builder/typegen does not read these dirs
/// directly; it consumes approved snapshots projected into the build root.
pub const BUILDER_EXTERNAL_TOOLS_ENV: &str = "BAML_EXTERNAL_TOOLS_DIR";

/// Build a [`CompositeCatalog`] for the builder: inventory first, then
/// approved external-tool snapshots and MCP snapshots.
pub fn build_builder_catalog() -> Result<CompositeCatalog> {
    build_builder_catalog_with_roots(None, None)
}

pub fn build_builder_catalog_with_mcp_root(mcp_root: Option<&Path>) -> Result<CompositeCatalog> {
    build_builder_catalog_with_roots(mcp_root, None)
}

pub fn build_builder_catalog_with_roots(
    mcp_root: Option<&Path>,
    external_cache_root: Option<&Path>,
) -> Result<CompositeCatalog> {
    let inventory = InventoryCatalog::new();

    let snapshot_external = match external_cache_root {
        Some(root) => {
            let catalog = ExternalToolSnapshotCatalog::from_root(root)?;
            if catalog.is_empty() {
                None
            } else {
                Some(catalog)
            }
        }
        None => ExternalToolSnapshotCatalog::from_env()?.filter(|c| !c.is_empty()),
    };

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
    if let Some(ext) = &snapshot_external {
        for meta in ext.iter() {
            if !existing_names.insert(meta.name.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "External tool snapshot name collision at build time: '{}' already exists in inventory.",
                    meta.name
                )));
            }
        }
    }
    if let Some(mcp) = &mcp {
        for meta in mcp.iter() {
            if !existing_names.insert(meta.name.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "MCP tool name collision at build time: '{}' already exists in inventory or external snapshots.",
                    meta.name
                )));
            }
        }
    }

    let mut composite = CompositeCatalog::new();
    composite.add(Box::new(inventory));
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
