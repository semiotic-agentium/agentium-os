// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::Path};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ExternalToolManifest, ExternalToolMetadata, MetadataSchemas, ToolRuntime, ToolSchemaResult,
    metadata::metadata_schema_digest, runtime::DEFAULT_PROCESS_COMMAND,
};
use crate::{
    approval::{ApprovalRecord, ApprovalState},
    mcp_snapshot::{Digest, canonical_digest},
};

pub const EXTERNAL_TOOL_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const EXTERNAL_TOOL_SOURCE: &str = "external_tool";

pub type ExternalApprovalState = ApprovalState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolSnapshot {
    pub snapshot_schema_version: u32,
    pub source: String,
    pub snapshot_digest: Digest,
    pub tool: ExternalToolMetadata,
    pub describe: ExternalToolDescribeSnapshot,
    pub digests: ExternalToolSnapshotDigests,
    pub approval: ApprovalRecord<ExternalApprovalState>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolDescribeSnapshot {
    pub protocol_version: String,
    pub supported_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_payload_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_digest: Option<Digest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolSnapshotDigests {
    pub manifest_digest: Digest,
    pub schema_digest: Digest,
    pub runtime_digest: Digest,
    pub snapshot_digest: Digest,
}

impl ExternalToolSnapshot {
    pub fn from_parts(
        tool_dir: &Path,
        manifest: ExternalToolManifest,
        schema: ToolSchemaResult,
        describe: ExternalToolDescribeSnapshot,
        created_at: impl Into<String>,
    ) -> Result<Self> {
        if schema.tool_name != manifest.name {
            return Err(BamlRtError::InvalidArgument(format!(
                "tool/schema returned tool_name '{}' but manifest declares '{}'",
                schema.tool_name, manifest.name
            )));
        }
        if schema.content_type != "application/schema+json" {
            return Err(BamlRtError::InvalidArgument(format!(
                "tool/schema for '{}' returned unsupported content_type '{}'",
                schema.tool_name, schema.content_type
            )));
        }

        let schemas = MetadataSchemas {
            input: schema.input,
            output: schema.output,
        };
        let mut tool = manifest.clone().into_metadata(schemas);
        inline_coordination_baml(tool_dir, &mut tool)?;

        let schema_digest = compute_external_schema_digest(&tool);
        let content_digest = parse_digest(&schema.content_digest, "tool/schema content_digest")?;
        if content_digest != schema_digest {
            return Err(BamlRtError::InvalidArgument(format!(
                "tool/schema content_digest '{}' does not match computed digest '{}'",
                content_digest, schema_digest
            )));
        }
        if let Some(advertised) = describe.schema_digest
            && advertised != schema_digest
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "tool/describe schema_digest '{}' does not match computed digest '{}'",
                advertised, schema_digest
            )));
        }

        let manifest_digest = compute_manifest_digest(&manifest);
        let runtime_digest = compute_runtime_digest(tool.runtime.as_ref())?;
        // snapshot_digest is a placeholder here — compute_snapshot_digest nulls
        // it out before hashing so the initial value does not affect the result.
        // Both fields are overwritten immediately after.
        let mut snapshot = Self {
            snapshot_schema_version: EXTERNAL_TOOL_SNAPSHOT_SCHEMA_VERSION,
            source: EXTERNAL_TOOL_SOURCE.to_string(),
            snapshot_digest: schema_digest,
            tool,
            describe: ExternalToolDescribeSnapshot {
                schema_digest: Some(schema_digest),
                ..describe
            },
            digests: ExternalToolSnapshotDigests {
                manifest_digest,
                schema_digest,
                runtime_digest,
                snapshot_digest: schema_digest,
            },
            approval: ApprovalRecord::pending(),
            created_at: created_at.into(),
        };
        let snapshot_digest = compute_snapshot_digest(&snapshot)?;
        snapshot.snapshot_digest = snapshot_digest;
        snapshot.digests.snapshot_digest = snapshot_digest;
        Ok(snapshot)
    }
}

pub fn compute_external_schema_digest(tool: &ExternalToolMetadata) -> Digest {
    Digest::new(metadata_schema_digest(tool))
}

pub fn compute_manifest_digest(manifest: &ExternalToolManifest) -> Digest {
    canonical_digest(manifest)
}

pub fn compute_runtime_digest(runtime: Option<&ToolRuntime>) -> Result<Digest> {
    match runtime.cloned().unwrap_or_default() {
        ToolRuntime::Process(spec) => {
            let command = if spec.command.is_empty() {
                vec![DEFAULT_PROCESS_COMMAND.to_string()]
            } else {
                spec.command
            };
            Ok(canonical_digest(&json!({
                "kind": "process",
                "command": command,
                "setup": spec.setup,
            })))
        }
        ToolRuntime::Sandbox(spec) => Ok(canonical_digest(&json!({
            "kind": "sandbox",
            "image": spec.image,
            "entrypoint": spec.entrypoint,
            "adapter": spec.adapter,
        }))),
    }
}

pub fn validate_external_tool_snapshot(snapshot: &ExternalToolSnapshot) -> Result<()> {
    if snapshot.coordination_requires_inline_baml() {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' declares coordination but does not inline coordination_baml",
            snapshot.tool.name
        )));
    }

    let manifest = ExternalToolManifest::from(snapshot.tool.clone());
    let manifest_digest = compute_manifest_digest(&manifest);
    if snapshot.digests.manifest_digest != manifest_digest {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' manifest_digest mismatch: expected {}, computed {}",
            snapshot.tool.name, snapshot.digests.manifest_digest, manifest_digest
        )));
    }

    let schema_digest = compute_external_schema_digest(&snapshot.tool);
    if snapshot.digests.schema_digest != schema_digest {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' schema_digest mismatch: expected {}, computed {}",
            snapshot.tool.name, snapshot.digests.schema_digest, schema_digest
        )));
    }
    if let Some(describe_digest) = snapshot.describe.schema_digest
        && describe_digest != schema_digest
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' describe schema_digest mismatch: expected {}, computed {}",
            snapshot.tool.name, describe_digest, schema_digest
        )));
    }

    let runtime_digest = compute_runtime_digest(snapshot.tool.runtime.as_ref())?;
    if snapshot.digests.runtime_digest != runtime_digest {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' runtime_digest mismatch: expected {}, computed {}",
            snapshot.tool.name, snapshot.digests.runtime_digest, runtime_digest
        )));
    }

    let snapshot_digest = compute_snapshot_digest(snapshot)?;
    if snapshot.snapshot_digest != snapshot_digest
        || snapshot.digests.snapshot_digest != snapshot_digest
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' snapshot_digest mismatch: expected {}, computed {}",
            snapshot.tool.name, snapshot.snapshot_digest, snapshot_digest
        )));
    }

    Ok(())
}

impl ExternalToolSnapshot {
    fn coordination_requires_inline_baml(&self) -> bool {
        self.tool.coordination.is_some() && self.tool.coordination_baml.is_none()
    }
}

pub fn compute_snapshot_digest(snapshot: &ExternalToolSnapshot) -> Result<Digest> {
    let mut value =
        serde_json::to_value(snapshot).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: "failed to serialize external tool snapshot".to_string(),
            source: Box::new(e),
        })?;
    if let Value::Object(map) = &mut value {
        map.remove("approval");
        map.insert("snapshot_digest".to_string(), Value::Null);
        if let Some(Value::Object(digests)) = map.get_mut("digests") {
            digests.insert("snapshot_digest".to_string(), Value::Null);
        }
    }
    Ok(canonical_digest(&value))
}

pub fn validate_describe_schema_support(
    tool_name: &str,
    describe: &super::ToolDescribeResult,
) -> Result<ExternalToolDescribeSnapshot> {
    if describe.tool_name != tool_name {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool/describe returned tool_name '{}' but expected '{}'",
            describe.tool_name, tool_name
        )));
    }
    if !describe
        .supported_methods
        .iter()
        .any(|method| method == super::METHOD_SCHEMA)
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool '{}' does not advertise {}",
            tool_name,
            super::METHOD_SCHEMA
        )));
    }
    Ok(ExternalToolDescribeSnapshot {
        protocol_version: describe.protocol_version.clone(),
        supported_methods: describe.supported_methods.clone(),
        max_payload_bytes: describe.max_payload_bytes,
        schema_digest: describe
            .schema_digest
            .as_deref()
            .map(|value| parse_digest(value, "tool/describe schema_digest"))
            .transpose()?,
    })
}

fn inline_coordination_baml(tool_dir: &Path, tool: &mut ExternalToolMetadata) -> Result<()> {
    let Some(spec) = &tool.coordination else {
        return Ok(());
    };
    let path = tool_dir.join(&spec.baml_file);
    let body = fs::read_to_string(&path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
        message: format!(
            "tool '{}': failed to read coordination BAML at {}",
            tool.name,
            path.display()
        ),
        source: Box::new(e),
    })?;
    tool.coordination_baml = Some(body);
    Ok(())
}

fn parse_digest(value: &str, label: &str) -> Result<Digest> {
    value
        .parse()
        .map_err(|e| BamlRtError::InvalidArgument(format!("invalid {label}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ToolAccess,
        external_tools::{InvocationMode, ToolDescribeResult},
    };

    fn manifest() -> ExternalToolManifest {
        ExternalToolManifest {
            tool_abi_version: "1".to_string(),
            name: "support/echo".to_string(),
            description: "echo".to_string(),
            bundle: "support".to_string(),
            local_name: "echo".to_string(),
            access_level: ToolAccess::Read,
            tags: vec![],
            invocation_mode: InvocationMode::SingleShot,
            session_policy: Default::default(),
            secrets: vec![],
            secret_scope: Default::default(),
            capabilities: json!({}),
            config_bundle: None,
            runtime: None,
            coordination: None,
        }
    }

    fn schema_for(input: serde_json::Value, output: serde_json::Value) -> ToolSchemaResult {
        let meta = manifest().into_metadata(MetadataSchemas {
            input: input.clone(),
            output: output.clone(),
        });
        let content_digest = compute_external_schema_digest(&meta).to_string();
        ToolSchemaResult {
            schema_version: 1,
            tool_name: "support/echo".to_string(),
            content_type: "application/schema+json".to_string(),
            content_digest,
            input,
            output,
        }
    }

    #[test]
    fn snapshot_digest_ignores_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let describe = ExternalToolDescribeSnapshot {
            protocol_version: "1".to_string(),
            supported_methods: vec![super::super::METHOD_SCHEMA.to_string()],
            max_payload_bytes: None,
            schema_digest: None,
        };
        let mut snap = ExternalToolSnapshot::from_parts(
            tmp.path(),
            manifest(),
            schema_for(json!({"type": "object"}), json!({"type": "object"})),
            describe,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let before = compute_snapshot_digest(&snap).unwrap();
        snap.approval.state = ApprovalState::Approved;
        snap.approval.owner = Some("ops".to_string());
        assert_eq!(before, compute_snapshot_digest(&snap).unwrap());
    }

    #[test]
    fn validate_describe_rejects_missing_schema_method() {
        let describe = ToolDescribeResult {
            protocol_version: "1".into(),
            tool_name: "support/echo".into(),
            supported_methods: vec!["tool/describe".into(), "tool/invoke".into()],
            max_payload_bytes: None,
            schema_digest: None,
            capabilities: None,
        };
        assert!(validate_describe_schema_support("support/echo", &describe).is_err());
    }
}
