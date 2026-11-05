// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External tools: approved snapshots, `.ktool` bundles, and the v2 hosts
//! that serve them (Sandbox VMs, Trusted processes, Service endpoints).
//!
//! Every external tool reaches the runtime as an approved repository
//! snapshot pinned by the agent's lockfile, and runs through
//! [`host::ToolHost`] or [`host::ServiceHost`]. Tool manifests in source dirs
//! are read only for webhook datasources ([`crate::external_datasource`]).

pub mod descriptor;
pub mod drift;
pub mod handler;
pub mod host;
pub mod input_nulls;
pub mod invoker;
pub mod ktool;
pub mod lockfile;
pub mod metadata;
pub mod metadata_catalog;
pub mod policy;
pub mod protocol;
pub mod registry_resolver;
pub mod runtime;
pub mod sandbox;
pub mod snapshot;
pub mod snapshot_catalog;

use std::sync::Arc;

pub use descriptor::{
    CapabilityRequest, DESCRIPTOR_ABI, HostPort, McpMappingError, OutputTrust, SecretBinding,
    SecretRequest, SideEffect, ToolDescriptor as UnifiedToolDescriptor, compute_descriptor_digest,
};
pub use drift::DriftGuard;
pub use handler::{ProcessToolHandler, ProcessToolSession, SessionInvokerFactory};
pub use host::{ServiceClient, ServiceHost};
pub use invoker::{
    ExternalInvoker, InvokeRequest, InvokeResponse, ToolDescribe, ToolInvoker, map_jsonrpc_error,
};
pub use ktool::{
    AuditedKtoolBundle, BundleFile, KTOOL_BUNDLE_SCHEMA_VERSION, KTOOL_DESCRIPTOR_PATH,
    KTOOL_MANIFEST_PATH, KTOOL_NODE_ENTRYPOINT, KTOOL_SBOM_PATH, KTOOL_TOOLS_PATH, KtoolManifest,
    KtoolRuntimeImage, KtoolRuntimeTier, KtoolToolEntry, KtoolTrustedLaunch,
    MAX_KTOOL_BUNDLE_BYTES, MAX_KTOOL_UNPACKED_BYTES, audit_ktool_bundle, build_ktool_bundle,
    build_ktool_bundle_for_tier, compute_bundle_digest, compute_tool_digest_v2,
    default_node_runtime_image, host_native_runtime_image, runtime_image_for_tier,
    verify_ktool_snapshot,
};
pub use lockfile::{
    EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalToolInterfaceLockEntry, ExternalToolsLockfile,
    ExternalToolsetLockEntry,
};
pub use metadata::{
    CoordinationSpec, ExternalDatasourceManifest, ExternalDatasourceMode, ExternalSecretScope,
    ExternalSessionPolicy, ExternalToolManifest, ExternalToolMetadata, InvocationMode,
    MetadataSchemas, read_external_manifest,
};
pub use metadata_catalog::{
    build_builder_catalog, build_builder_catalog_with_mcp_root, build_builder_catalog_with_roots,
    build_builder_catalog_with_static_catalog,
};
pub use policy::{
    BACKOFF_SCHEDULE_MS, DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT, DEFAULT_MAX_CONCURRENT,
    DEFAULT_QUARANTINE_THRESHOLD, InvocationPolicy, PolicyError, QuarantineState, ToolQuota,
};
pub use protocol::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR,
    ERR_PAYLOAD_LIMIT_EXCEEDED, ERR_SCHEMA_DIGEST_MISMATCH, ERR_SIDECAR_MALFORMED,
    ERR_SIDECAR_MISSING, ERR_SIDECAR_SCHEMA_INVALID, ERR_SIDECAR_SIZE_EXCEEDED,
    ERR_UNSUPPORTED_PROTOCOL, ErrorClass, EventSchema, JsonRpcError, JsonRpcRequest,
    JsonRpcResponse, METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA, PROTOCOL_VERSION,
    SUPPORTED_METHODS, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult, ToolSchemaResult,
};
pub use registry_resolver::ExternalRegistryResolver;
pub use runtime::{
    DEFAULT_PROCESS_COMMAND, ProcessRuntimeSpec, SandboxAdapterRuntimeSpec, SandboxImageRef,
    SandboxRuntimeSpec, ServiceEndpoint, ServiceRuntimeSpec, ToolRuntime, ToolRuntimeKind,
    TrustedLaunch, TrustedRuntimeSpec,
};
use serde_json::Value;
pub use snapshot::{
    DescribeProvenance, EXTERNAL_TOOL_SNAPSHOT_SCHEMA_VERSION, EXTERNAL_TOOL_SOURCE,
    ExternalApprovalState, ExternalToolDescribeSnapshot, ExternalToolSnapshot,
    ExternalToolSnapshotDigests, ExternalToolV2Snapshot, ServiceEndpointRef, ToolDelivery,
    ToolsetApproval, ToolsetPublishRecord, ToolsetSnapshot, compute_descriptor_schema_digest,
    compute_external_schema_digest, compute_manifest_digest, compute_runtime_digest,
    compute_schema_digest, compute_snapshot_digest, format_unix_rfc3339_utc,
    now_snapshot_timestamp, validate_describe_schema_support, validate_external_tool_snapshot,
};
pub use snapshot_catalog::{ExternalToolRegistryCatalog, ExternalToolSnapshotCatalog};

#[derive(Debug, Clone)]
pub enum ExternalLifecycleEvent {
    Describe {
        tool_name: String,
        identity: Option<String>,
        protocol_version: Option<String>,
        latency_ms: u64,
        result: String,
        details: Value,
    },
    Artifact {
        tool_name: String,
        artifact_ref: String,
        digest: Option<String>,
        signer: Option<String>,
        verification_result: String,
        pull_latency_ms: Option<u64>,
        details: Value,
    },
    Quarantine {
        tool_name: String,
        reason: String,
        consecutive_failures: u32,
        started_at_ms: u64,
    },
    QuarantineLifted {
        tool_name: String,
        lifted_by: String,
        lifted_at_ms: u64,
    },
    /// The live tool's `tool/schema` no longer matches its approved snapshot
    /// digest, detected lazily on first invoke by [`drift::DriftGuard`]. The
    /// invoke fails closed; this event records the drift for provenance/audit.
    SchemaDrift {
        tool_name: String,
        expected_schema_digest: String,
        observed_schema_digest: String,
    },
    /// An operator granted a specific agent access to an exact Trusted bundle.
    TrustGranted {
        tool_name: String,
        agent_id: String,
        tool_digest: String,
        node: String,
    },
    /// An operator revoked access to an exact Trusted bundle.
    TrustRevoked {
        tool_name: String,
        agent_id: String,
        tool_digest: String,
        node: String,
    },
    /// A Trusted invocation ran under the operator-trust authority. The call
    /// identity is carried as facts so audit consumers never infer lineage from
    /// tool names or ID prefixes.
    TrustedInvoke {
        tool_name: String,
        agent_id: String,
        tool_digest: String,
        node: String,
        artifact_hash: String,
        context_id: String,
        task_id: String,
        invocation_id: String,
        trace_id: String,
    },
    /// A Service toolset passed its live endpoint verification at deploy.
    ServiceBound { tool_name: String, endpoint: String },
}

pub type ExternalLifecycleRecorder = Arc<dyn Fn(ExternalLifecycleEvent) + Send + Sync>;
