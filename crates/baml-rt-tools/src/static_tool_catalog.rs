// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Static tool catalog: a serializable snapshot of the static (compiled-in)
//! tool metadata a runner links via `inventory`, plus a [`ToolCatalog`] that
//! reconstructs from it.
//!
//! This decouples `regen`/typegen from the CLI's own link-time inventory: the
//! runner becomes the source of truth for which static tools exist. The per-tool
//! shape is the existing [`ToolFunctionMetadataExport`] (which now carries every
//! `ToolFunctionMetadata` field), so reconstruction is lossless and typegen
//! treats a fetched catalog identically to local inventory.
//!
//! Symmetric with the external-tool snapshot catalogs in
//! [`crate::external_tools::snapshot_catalog`]; served at
//! `GET /repository/static-tools/snapshots`.

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};

use crate::{
    ToolName,
    session_coordination::render_inventory_fragment,
    tool_catalog::{InventoryCatalog, ToolCatalog},
    tools::{ToolFunctionMetadata, ToolFunctionMetadataExport},
};

/// Schema-version tag for [`StaticToolCatalogResponse`]. Bumped only on
/// breaking changes to the wire shape; consumers reject unknown versions.
pub const STATIC_TOOL_CATALOG_SCHEMA_VERSION: &str = "static-tool-catalog.v1";

/// Serializable catalog of a runner's static (compiled-in) tools.
///
/// `runner_version` and `git_sha` are advisory build identifiers, included only
/// so a missing-tool failure can name the runner build and the operator can pin
/// a compatible one. We deliberately do **not** expose the enabled cargo-feature
/// set: it leaks the build's capability vocabulary (including gated tools the
/// caller can't otherwise see) without helping an authorized caller, who already
/// has the full `tools` list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticToolCatalogResponse {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    pub tools: Vec<ToolFunctionMetadataExport>,
}

impl StaticToolCatalogResponse {
    /// Project any [`ToolCatalog`] into the wire response, stamping build
    /// identity. Tools are sorted by name for stable output.
    ///
    /// Static/internal tools carry session-coordination fragments in the
    /// inventory provider channel, not in `ToolFunctionMetadata`. Inline those
    /// fragments into `coordination_baml` before serializing so a thin CLI can
    /// reconstruct the same prelude without linking the tool crate.
    pub fn from_catalog<C: ToolCatalog>(
        catalog: &C,
        runner_version: Option<String>,
        git_sha: Option<String>,
    ) -> Result<Self> {
        let mut tools: Vec<ToolFunctionMetadataExport> = catalog
            .iter()
            .map(|metadata| {
                let mut export = ToolFunctionMetadataExport::from(metadata);
                if export.coordination_baml.is_none() {
                    export.coordination_baml = render_inventory_fragment(&export.name.to_string())?;
                }
                Ok(export)
            })
            .collect::<Result<_>>()?;
        tools.sort_by_key(|tool| tool.name.to_string());
        Ok(Self {
            schema_version: STATIC_TOOL_CATALOG_SCHEMA_VERSION.to_string(),
            runner_version,
            git_sha,
            tools,
        })
    }

    /// Project the current process's linked `inventory` into the wire response.
    /// The runner calls this so the response reflects the slim-runner reality
    /// (only the static tools actually compiled into that binary).
    pub fn from_inventory(runner_version: Option<String>, git_sha: Option<String>) -> Result<Self> {
        Self::from_catalog(&InventoryCatalog::new(), runner_version, git_sha)
    }
}

/// Builder/typegen catalog backed by a runner-fetched (or file-loaded)
/// [`StaticToolCatalogResponse`].
///
/// Reconstructs [`ToolFunctionMetadata`] losslessly and answers [`ToolCatalog`]
/// lookups.
#[derive(Debug, Clone, Default)]
pub struct StaticToolSnapshotCatalog {
    tools: Vec<ToolFunctionMetadata>,
}

impl StaticToolSnapshotCatalog {
    /// Reconstruct from a fetched/loaded response. Rejects unknown schema
    /// versions so a newer runner contract fails loudly rather than silently
    /// dropping fields.
    pub fn from_response(response: StaticToolCatalogResponse) -> Result<Self> {
        if response.schema_version != STATIC_TOOL_CATALOG_SCHEMA_VERSION {
            return Err(BamlRtError::InvalidArgument(format!(
                "unsupported static tool catalog schema version '{}' (expected '{}')",
                response.schema_version, STATIC_TOOL_CATALOG_SCHEMA_VERSION
            )));
        }
        let tools = response
            .tools
            .into_iter()
            .map(ToolFunctionMetadata::from)
            .collect();
        Ok(Self { tools })
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl ToolCatalog for StaticToolSnapshotCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
        self.tools.iter().find(|tool| &tool.name == name)
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
        Box::new(self.tools.iter())
    }
    // `bundle_config` uses the trait default: config_bundle is reconstructed
    // losslessly from the export, so the first matching tool resolves correctly.
}
