// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ExternalToolManifest, ExternalToolMetadata, KtoolRuntimeImage, MetadataSchemas,
    ServiceEndpoint, ServiceRuntimeSpec, SideEffect, ToolRuntime, ToolSchemaResult,
    UnifiedToolDescriptor, compute_descriptor_digest, metadata::metadata_schema_digest,
    runtime::DEFAULT_PROCESS_COMMAND,
};
use crate::{
    ToolAccess,
    approval::{ApprovalRecord, ApprovalState},
    mcp_snapshot::{Digest, canonical_digest},
};

pub const EXTERNAL_TOOL_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const EXTERNAL_TOOL_SOURCE: &str = "external_tool";

pub type ExternalApprovalState = ApprovalState;

/// Current wall-clock as an RFC3339 UTC timestamp string, the canonical format
/// for external-tool snapshot `created_at` / `reviewed_at` fields.
///
/// Shared by every snapshot producer (CLI local discovery and the runner-owned
/// enable endpoint) so a single field never carries two formats across paths.
pub fn now_snapshot_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_unix_rfc3339_utc(secs)
}

/// Format Unix seconds as `YYYY-MM-DDThh:mm:ssZ` without pulling in a date
/// crate. Uses Howard Hinnant's civil-from-days algorithm.
pub fn format_unix_rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolSnapshot {
    pub snapshot_schema_version: u32,
    pub source: String,
    pub snapshot_digest: Digest,
    pub tool: ExternalToolMetadata,
    pub describe: ExternalToolDescribeSnapshot,
    pub digests: ExternalToolSnapshotDigests,
    /// V2 executable identity. Absent on legacy discovery snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2: Option<ExternalToolV2Snapshot>,
    pub approval: ApprovalRecord<ExternalApprovalState>,
    pub created_at: String,
}

/// V2 contract and executable-byte identity stored with the approval snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalToolV2Snapshot {
    pub descriptor: UnifiedToolDescriptor,
    pub descriptor_digest: Digest,
    pub delivery: ToolDelivery,
}

/// How a v2 tool reaches its execution environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolDelivery {
    /// Sandbox and Trusted tools carry an audited `.ktool` bundle.
    Bundled {
        bundle_digest: Digest,
        tool_digest: Digest,
        runtime_image: KtoolRuntimeImage,
    },
    /// Service tools carry no executable bytes. The endpoint is resolved by the
    /// operator at deploy time.
    Reference {
        endpoint: ServiceEndpointRef,
        descriptor_source: DescribeProvenance,
    },
}

/// Opaque endpoint reference stored in a Service snapshot.
///
/// The endpoint is deliberately separate from the runtime declaration so the
/// published snapshot remains the identity of one approved service reference.
pub type ServiceEndpointRef = ServiceEndpoint;

/// Records how the descriptor was obtained during Service publish.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DescribeProvenance {
    /// The descriptor came from an MCP `initialize` plus `tools/list` exchange.
    McpToolsList {
        protocol_version: String,
        server_identity_digest: Digest,
        tools_digest: Digest,
        tool_name: String,
    },
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
        validate_datasource_event_contract(&manifest, &schema)?;

        let schemas = MetadataSchemas {
            input: schema.input,
            output: schema.output,
            events: schema.events,
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
            v2: None,
            approval: ApprovalRecord::pending(),
            created_at: created_at.into(),
        };
        let snapshot_digest = compute_snapshot_digest(&snapshot)?;
        snapshot.snapshot_digest = snapshot_digest;
        snapshot.digests.snapshot_digest = snapshot_digest;
        Ok(snapshot)
    }

    /// Build an approved v2 snapshot from a host-audited `.ktool` bundle.
    pub fn from_v2_bundle(
        descriptor: UnifiedToolDescriptor,
        bundle_digest: Digest,
        tool_digest: Digest,
        runtime_image: KtoolRuntimeImage,
        approved_by: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Result<Self> {
        let descriptor_digest = compute_descriptor_digest(&descriptor);
        let tool = metadata_from_v2_descriptor(&descriptor)?;
        let created_at = created_at.into();
        let approval = ApprovalRecord {
            state: ApprovalState::Approved,
            owner: Some(approved_by.into()),
            reviewed_at: Some(created_at.clone()),
            expires_at: None,
        };
        let mut snapshot = Self {
            snapshot_schema_version: EXTERNAL_TOOL_SNAPSHOT_SCHEMA_VERSION,
            source: EXTERNAL_TOOL_SOURCE.to_string(),
            snapshot_digest: descriptor_digest,
            tool,
            describe: ExternalToolDescribeSnapshot {
                protocol_version: "2".to_string(),
                supported_methods: vec!["load_tool".to_string(), "invoke".to_string()],
                max_payload_bytes: None,
                schema_digest: Some(descriptor_digest),
            },
            // Keep the legacy registry columns populated during the v1→v2
            // storage migration. The full v2 identities above are authoritative.
            digests: ExternalToolSnapshotDigests {
                manifest_digest: descriptor_digest,
                schema_digest: descriptor_digest,
                runtime_digest: runtime_image.digest,
                snapshot_digest: descriptor_digest,
            },
            v2: Some(ExternalToolV2Snapshot {
                descriptor,
                descriptor_digest,
                delivery: ToolDelivery::Bundled {
                    bundle_digest,
                    tool_digest,
                    runtime_image,
                },
            }),
            approval,
            created_at,
        };
        let snapshot_digest = compute_snapshot_digest(&snapshot)?;
        snapshot.snapshot_digest = snapshot_digest;
        snapshot.digests.snapshot_digest = snapshot_digest;
        Ok(snapshot)
    }

    /// Build an approved Service snapshot from a descriptor obtained from the
    /// referenced endpoint. Service snapshots never carry bundle bytes.
    pub fn from_v2_service(
        descriptor: UnifiedToolDescriptor,
        endpoint: ServiceEndpointRef,
        descriptor_source: DescribeProvenance,
        approved_by: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Result<Self> {
        let descriptor_digest = compute_descriptor_digest(&descriptor);
        let mut tool = metadata_from_v2_descriptor(&descriptor)?;
        tool.runtime = Some(ToolRuntime::Service(ServiceRuntimeSpec {
            endpoint: endpoint.clone(),
        }));
        let schema_digest = compute_descriptor_schema_digest(&descriptor);
        let endpoint_digest = canonical_digest(&endpoint);
        let created_at = created_at.into();
        let approval = ApprovalRecord {
            state: ApprovalState::Approved,
            owner: Some(approved_by.into()),
            reviewed_at: Some(created_at.clone()),
            expires_at: None,
        };
        let mut snapshot = Self {
            snapshot_schema_version: EXTERNAL_TOOL_SNAPSHOT_SCHEMA_VERSION,
            source: EXTERNAL_TOOL_SOURCE.to_string(),
            snapshot_digest: descriptor_digest,
            tool,
            describe: ExternalToolDescribeSnapshot {
                protocol_version: "2".to_string(),
                supported_methods: vec!["describe".to_string(), "invoke".to_string()],
                max_payload_bytes: None,
                schema_digest: Some(schema_digest),
            },
            digests: ExternalToolSnapshotDigests {
                manifest_digest: descriptor_digest,
                schema_digest,
                runtime_digest: endpoint_digest,
                snapshot_digest: descriptor_digest,
            },
            v2: Some(ExternalToolV2Snapshot {
                descriptor,
                descriptor_digest,
                delivery: ToolDelivery::Reference {
                    endpoint,
                    descriptor_source,
                },
            }),
            approval,
            created_at,
        };
        let snapshot_digest = compute_snapshot_digest(&snapshot)?;
        snapshot.snapshot_digest = snapshot_digest;
        snapshot.digests.snapshot_digest = snapshot_digest;
        validate_external_tool_snapshot(&snapshot)?;
        Ok(snapshot)
    }
}

/// Digest the input/output part of a descriptor using the legacy schema-drift
/// contract. It is kept alongside the full descriptor digest because a Service
/// re-checks the live endpoint's callable schema before its first invoke.
pub fn compute_descriptor_schema_digest(descriptor: &UnifiedToolDescriptor) -> Digest {
    compute_schema_digest(&descriptor.input_schema, &descriptor.output_schema)
}

pub fn compute_schema_digest(input: &Value, output: &Value) -> Digest {
    canonical_digest(&json!({
        "input": input,
        "output": output,
    }))
}

fn metadata_from_v2_descriptor(descriptor: &UnifiedToolDescriptor) -> Result<ExternalToolMetadata> {
    let (bundle, local_name) = descriptor.name.split_once('/').ok_or_else(|| {
        BamlRtError::InvalidArgument(format!(
            "tool descriptor name '{}' must be bundle/local_name",
            descriptor.name
        ))
    })?;
    if bundle.is_empty() || local_name.is_empty() || local_name.contains('/') {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool descriptor name '{}' must be bundle/local_name",
            descriptor.name
        )));
    }

    Ok(ExternalToolMetadata {
        tool_abi_version: "2".to_string(),
        name: descriptor.name.clone(),
        description: descriptor.description.clone(),
        bundle: bundle.to_string(),
        local_name: local_name.to_string(),
        access_level: match descriptor.side_effect {
            SideEffect::Read => ToolAccess::Read,
            SideEffect::Write => ToolAccess::Write,
            SideEffect::Irreversible => ToolAccess::Delete,
        },
        tags: Vec::new(),
        invocation_mode: super::InvocationMode::SingleShot,
        session_policy: Default::default(),
        schemas: MetadataSchemas {
            input: descriptor.input_schema.clone(),
            output: descriptor.output_schema.clone(),
            events: Vec::new(),
        },
        event_sources: Vec::new(),
        datasources: Vec::new(),
        secrets: descriptor
            .requested
            .secrets
            .iter()
            .map(|request| request.name.clone())
            .collect(),
        secret_scope: Default::default(),
        capabilities: serde_json::to_value(&descriptor.requested).map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "failed to serialize v2 capability requests".to_string(),
                source: Box::new(e),
            }
        })?,
        config_bundle: None,
        // P3 dispatches v2 snapshots by `v2.delivery`; the retired v1
        // process/sandbox declaration must not decide how bundled bytes run.
        runtime: None,
        coordination: None,
        coordination_baml: None,
    })
}

fn validate_datasource_event_contract(
    manifest: &ExternalToolManifest,
    schema: &ToolSchemaResult,
) -> Result<()> {
    if manifest.datasources.is_empty() {
        return Ok(());
    }
    if schema.events.is_empty() {
        return Err(BamlRtError::InvalidArgument(format!(
            "tool/schema for datasource tool '{}' must return at least one events[] schema",
            manifest.name
        )));
    }

    for datasource in &manifest.datasources {
        if let Some(schema_version) = &datasource.schema_version
            && !schema
                .events
                .iter()
                .any(|event| &event.schema_version == schema_version)
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "datasource '{}.{}' declares schema_version '{}' but tool/schema returned no matching events[] schema",
                manifest.name, datasource.key, schema_version
            )));
        }
    }
    Ok(())
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
        ToolRuntime::Trusted(spec) => Ok(canonical_digest(&json!({
            "kind": "trusted",
            "launch": spec.launch,
            "node": spec.node,
        }))),
        ToolRuntime::Service(spec) => Ok(canonical_digest(&json!({
            "kind": "service",
            "endpoint": spec.endpoint,
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
    if let Some(v2) = &snapshot.v2 {
        return validate_v2_snapshot(snapshot, v2);
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

fn validate_v2_snapshot(
    snapshot: &ExternalToolSnapshot,
    v2: &ExternalToolV2Snapshot,
) -> Result<()> {
    let descriptor_digest = compute_descriptor_digest(&v2.descriptor);
    if v2.descriptor_digest != descriptor_digest {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' descriptor_digest mismatch: expected {}, computed {}",
            snapshot.tool.name, v2.descriptor_digest, descriptor_digest
        )));
    }
    let projected = metadata_from_v2_descriptor(&v2.descriptor)?;
    if snapshot.tool.name != projected.name
        || snapshot.tool.description != projected.description
        || snapshot.tool.access_level != projected.access_level
        || snapshot.tool.schemas.input != projected.schemas.input
        || snapshot.tool.schemas.output != projected.schemas.output
        || snapshot.tool.secrets != projected.secrets
        || snapshot.tool.capabilities != projected.capabilities
    {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' metadata does not match its v2 descriptor",
            snapshot.tool.name
        )));
    }
    if snapshot.digests.manifest_digest != descriptor_digest {
        return Err(BamlRtError::InvalidArgument(format!(
            "external tool snapshot '{}' v2 manifest digest does not match its descriptor",
            snapshot.tool.name
        )));
    }
    match &v2.delivery {
        ToolDelivery::Bundled { runtime_image, .. } => {
            if snapshot.digests.schema_digest != descriptor_digest
                || snapshot.digests.runtime_digest != runtime_image.digest
                || snapshot.describe.schema_digest != Some(descriptor_digest)
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool snapshot '{}' bundled compatibility digests do not match its identities",
                    snapshot.tool.name
                )));
            }
        }
        ToolDelivery::Reference {
            endpoint,
            descriptor_source,
        } => {
            let endpoint_digest = canonical_digest(endpoint);
            if snapshot.tool.runtime
                != Some(ToolRuntime::Service(ServiceRuntimeSpec {
                    endpoint: endpoint.clone(),
                }))
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool snapshot '{}' Service runtime does not match its reference",
                    snapshot.tool.name
                )));
            }
            let expected_local_name = v2
                .descriptor
                .name
                .rsplit_once('/')
                .map(|(_, local)| local)
                .unwrap_or_default();
            let DescribeProvenance::McpToolsList { tool_name, .. } = descriptor_source;
            if tool_name != expected_local_name {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool snapshot '{}' descriptor provenance names '{}', expected '{}'",
                    snapshot.tool.name, tool_name, expected_local_name
                )));
            }
            let schema_digest = compute_descriptor_schema_digest(&v2.descriptor);
            if snapshot.digests.schema_digest != schema_digest
                || snapshot.digests.runtime_digest != endpoint_digest
                || snapshot.describe.schema_digest != Some(schema_digest)
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool snapshot '{}' service reference digests do not match its identities",
                    snapshot.tool.name
                )));
            }
            if matches!(endpoint, ServiceEndpoint::Mcp { url } if url.trim().is_empty()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "external tool snapshot '{}' has an empty service endpoint",
                    snapshot.tool.name
                )));
            }
        }
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
        external_tools::{
            CapabilityRequest, DESCRIPTOR_ABI, EventSchema, ExternalDatasourceManifest,
            ExternalDatasourceMode, InvocationMode, OutputTrust, ServiceEndpoint, SideEffect,
            ToolDescribeResult, UnifiedToolDescriptor,
            metadata::{schema_digest_from_events, schema_digest_from_io},
        },
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
            event_sources: vec![],
            datasources: vec![],
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
            events: Vec::new(),
        });
        let content_digest = compute_external_schema_digest(&meta).to_string();
        ToolSchemaResult {
            schema_version: 1,
            tool_name: "support/echo".to_string(),
            content_type: "application/schema+json".to_string(),
            content_digest,
            input,
            output,
            events: Vec::new(),
        }
    }

    fn datasource_manifest() -> ExternalToolManifest {
        let mut manifest = manifest();
        manifest.event_sources = vec!["echo".to_string()];
        manifest.datasources = vec![ExternalDatasourceManifest {
            key: "echo-webhook".to_string(),
            kind: "webhook".to_string(),
            mode: ExternalDatasourceMode::Raw,
            source_kind: None,
            schema_version: Some("echo.v1".to_string()),
            source_key: Some("echo:local".to_string()),
            response_status: None,
            dedupe_header: None,
            max_body_bytes: None,
            timeout_ms: None,
            methods: Vec::new(),
            handler: None,
        }];
        manifest
    }

    fn echo_event_schema() -> EventSchema {
        EventSchema {
            schema_version: "echo.v1".to_string(),
            name: Some("EchoEvent".to_string()),
            schema: json!({"type": "object", "additionalProperties": true}),
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
    fn normal_tool_schema_digest_stays_input_output_only() {
        let input = json!({"type": "object", "properties": {"q": {"type": "string"}}});
        let output = json!({"type": "object", "properties": {"ok": {"type": "boolean"}}});
        let meta = manifest().into_metadata(MetadataSchemas {
            input: input.clone(),
            output: output.clone(),
            events: vec![echo_event_schema()],
        });

        assert_eq!(
            compute_external_schema_digest(&meta).to_string(),
            schema_digest_from_io(&input, &output)
        );
    }

    #[test]
    fn datasource_schema_digest_uses_event_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = datasource_manifest();
        let events = vec![echo_event_schema()];
        let schema = ToolSchemaResult {
            schema_version: 1,
            tool_name: "support/echo".to_string(),
            content_type: "application/schema+json".to_string(),
            content_digest: schema_digest_from_events(&events),
            input: Value::Null,
            output: Value::Null,
            events: events.clone(),
        };
        let describe = ExternalToolDescribeSnapshot {
            protocol_version: "1".to_string(),
            supported_methods: vec![super::super::METHOD_SCHEMA.to_string()],
            max_payload_bytes: None,
            schema_digest: None,
        };

        let snapshot = ExternalToolSnapshot::from_parts(
            tmp.path(),
            manifest,
            schema,
            describe,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();

        assert_eq!(snapshot.tool.schemas.events, events);
        assert_eq!(
            snapshot.digests.schema_digest.to_string(),
            schema_digest_from_events(&snapshot.tool.schemas.events)
        );
    }

    #[test]
    fn service_delivery_round_trips_and_validates_without_bundle_identity() {
        let descriptor = UnifiedToolDescriptor {
            descriptor_abi: DESCRIPTOR_ABI,
            name: "support/remote_echo".into(),
            version: "1.0.0".into(),
            description: "remote echo".into(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            error_classes: vec![super::super::ErrorClass::Execution],
            side_effect: SideEffect::Read,
            idempotent: true,
            output_trust: OutputTrust::Untrusted,
            requested: CapabilityRequest::default(),
        };
        let snapshot = ExternalToolSnapshot::from_v2_service(
            descriptor,
            ServiceEndpoint::Mcp {
                url: "https://mcp.example.test/mcp".into(),
            },
            DescribeProvenance::McpToolsList {
                protocol_version: "2025-06-18".into(),
                server_identity_digest: canonical_digest(&json!({"server": "mcp"})),
                tools_digest: canonical_digest(&json!(["remote_echo"])),
                tool_name: "remote_echo".into(),
            },
            "ops",
            "2026-01-01T00:00:00Z",
        )
        .unwrap();

        validate_external_tool_snapshot(&snapshot).unwrap();
        let delivery = &snapshot.v2.as_ref().unwrap().delivery;
        assert!(matches!(delivery, ToolDelivery::Reference { .. }));
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: ExternalToolSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            serde_json::to_value(decoded).unwrap()
        );
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
