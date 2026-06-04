// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Build-time catalog source for approved external-tool snapshots.

use std::path::{Path, PathBuf};

use baml_rt_core::{BamlRtError, Result};

use super::{metadata::build_tool_metadata, snapshot::validate_external_tool_snapshot};
use crate::{
    ToolName, external_tool_cache, tool_catalog::ToolCatalog, tools::ToolFunctionMetadata,
};

/// Env var that points builder/runner at an external-tool snapshot cache root.
pub const BUILDER_EXTERNAL_TOOL_CACHE_ENV: &str = "BAML_EXTERNAL_TOOL_CACHE_DIR";

#[derive(Debug, Default)]
pub struct ExternalToolSnapshotCatalog {
    tools: Vec<ToolFunctionMetadata>,
}

impl ExternalToolSnapshotCatalog {
    pub fn from_root(root: &Path) -> Result<Self> {
        let snapshots = external_tool_cache::read_approved_snapshots(root)?;
        let mut tools = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for snapshot in snapshots {
            if !snapshot.approval.state.is_approved() {
                continue;
            }
            validate_external_tool_snapshot(&snapshot)?;
            let tool_name = ToolName::parse(&snapshot.tool.name)?;
            if !seen.insert(tool_name.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool snapshot '{}' loaded from {}",
                    tool_name,
                    root.display()
                )));
            }
            // Registry/cache snapshots must be source-dir independent. Validation above rejects
            // coordination specs unless `coordination_baml` was inlined at approval time, so an
            // empty source path cannot silently drop a referenced coordination file here.
            let mut metadata = build_tool_metadata(Path::new(""), &snapshot.tool, &tool_name)?;
            metadata.digest = Some(snapshot.digests.schema_digest.to_string());
            tools.push(metadata);
        }

        tools.sort_by_key(|tool| tool.name.to_string());
        Ok(Self { tools })
    }

    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var(BUILDER_EXTERNAL_TOOL_CACHE_ENV) {
            Ok(value) if !value.trim().is_empty() => {
                Self::from_root(&PathBuf::from(value)).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl ToolCatalog for ExternalToolSnapshotCatalog {
    fn by_name(&self, name: &ToolName) -> Option<&ToolFunctionMetadata> {
        self.tools.iter().find(|tool| &tool.name == name)
    }

    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a ToolFunctionMetadata> + 'a> {
        Box::new(self.tools.iter())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        ApprovalState,
        external_tool_cache::{write_approved_snapshot, write_pending_snapshot},
        external_tools::{
            ExternalToolDescribeSnapshot, ExternalToolManifest, ExternalToolSnapshot,
            MetadataSchemas, ToolSchemaResult,
        },
        tool_catalog::ToolCatalog,
        tools::{ToolAccess, ToolBackend},
    };

    fn manifest(name: &str) -> ExternalToolManifest {
        let (bundle, local_name) = name.split_once('/').unwrap();
        ExternalToolManifest {
            tool_abi_version: "1".to_string(),
            name: name.to_string(),
            description: format!("{name} description"),
            bundle: bundle.to_string(),
            local_name: local_name.to_string(),
            access_level: ToolAccess::Read,
            tags: vec!["snapshot".to_string()],
            invocation_mode: crate::external_tools::InvocationMode::SingleShot,
            session_policy: Default::default(),
            secrets: vec![],
            secret_scope: Default::default(),
            capabilities: json!({}),
            config_bundle: None,
            runtime: None,
            coordination: None,
        }
    }

    fn snapshot(name: &str, state: ApprovalState) -> ExternalToolSnapshot {
        let manifest = manifest(name);
        let input = json!({"type": "object", "properties": {"q": {"type": "string"}}});
        let output = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}});
        let metadata = manifest.clone().into_metadata(MetadataSchemas {
            input: input.clone(),
            output: output.clone(),
        });
        let schema = ToolSchemaResult {
            schema_version: 1,
            tool_name: name.to_string(),
            content_type: "application/schema+json".to_string(),
            content_digest: crate::external_tools::compute_external_schema_digest(&metadata)
                .to_string(),
            input,
            output,
        };
        let describe = ExternalToolDescribeSnapshot {
            protocol_version: "1".to_string(),
            supported_methods: vec![crate::external_tools::METHOD_SCHEMA.to_string()],
            max_payload_bytes: None,
            schema_digest: None,
        };
        let mut snapshot = ExternalToolSnapshot::from_parts(
            Path::new(""),
            manifest,
            schema,
            describe,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        snapshot.approval.state = state;
        snapshot
    }

    #[test]
    fn rejects_tampered_approved_snapshot_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let mut approved = snapshot("support/cache_tampered", ApprovalState::Approved);
        approved.tool.description = "tampered after approval".to_string();
        write_approved_snapshot(tmp.path(), &approved).unwrap();

        let err = ExternalToolSnapshotCatalog::from_root(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("digest mismatch"));
    }

    #[test]
    fn approved_snapshots_project_pending_and_stale_filter_out() {
        let tmp = tempfile::tempdir().unwrap();
        let approved = snapshot("support/cache_ok", ApprovalState::Approved);
        let stale = snapshot("support/cache_stale", ApprovalState::Stale);
        let pending = snapshot("support/cache_pending", ApprovalState::Approved);
        write_approved_snapshot(tmp.path(), &approved).unwrap();
        write_approved_snapshot(tmp.path(), &stale).unwrap();
        write_pending_snapshot(tmp.path(), &pending).unwrap();

        let catalog = ExternalToolSnapshotCatalog::from_root(tmp.path()).unwrap();
        assert_eq!(catalog.len(), 1);
        let name = ToolName::parse("support/cache_ok").unwrap();
        let meta = catalog.by_name(&name).unwrap();
        assert_eq!(meta.backend, ToolBackend::External);
        assert_eq!(meta.input_schema["properties"]["q"]["type"], "string");
        assert!(
            catalog
                .by_name(&ToolName::parse("support/cache_stale").unwrap())
                .is_none()
        );
        assert!(
            catalog
                .by_name(&ToolName::parse("support/cache_pending").unwrap())
                .is_none()
        );
    }
}
