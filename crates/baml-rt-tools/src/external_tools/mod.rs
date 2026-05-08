//! External tool protocol + execution backend.
//!
//! This module defines the wire contract (JSON-RPC `tool/describe` + `tool/invoke`)
//! and the abstractions needed to run tools out-of-process over stdio/UDS.
//!
//! Phase 1 scope: stateless, single-shot subprocess invocation. Later phases
//! extend the same protocol to Wasm or keep-alive transports.

pub mod handler;
pub mod invoker;
pub mod lockfile;
pub mod metadata;
pub mod metadata_catalog;
pub mod policy;
pub mod protocol;
pub mod resolver;
pub mod runtime;
pub mod runtime_lock;
pub mod sandbox;
pub mod session_handler;
pub mod session_invoker;
pub mod sidecar_bundle;
pub mod stdio;

use std::sync::Arc;

pub use handler::{ProcessToolHandler, ProcessToolSession};
pub use invoker::{
    ExternalInvoker, InvokeRequest, InvokeResponse, ToolDescribe, ToolInvoker, map_jsonrpc_error,
};
pub use lockfile::{
    EXTERNAL_TOOLS_LOCKFILE_NAME, ExternalLockfileMode, ExternalToolLockEntry,
    ExternalToolsLockfile,
};
pub use metadata::{
    CoordinationSpec, ExternalSecretScope, ExternalSessionPolicy, ExternalToolMetadata,
    InvocationMode, MetadataSchemas, compute_tool_digest, read_external_metadata,
    read_runtime_external_metadata,
};
pub use metadata_catalog::{
    BUILDER_EXTERNAL_TOOLS_ENV, ExternalMetadataCatalog, build_builder_catalog,
    external_dirs_from_env,
};
pub use policy::{
    BACKOFF_SCHEDULE_MS, DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT, DEFAULT_MAX_CONCURRENT,
    DEFAULT_QUARANTINE_THRESHOLD, InvocationPolicy, PolicyError, QuarantineState, ToolQuota,
};
pub use protocol::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR,
    ERR_PAYLOAD_LIMIT_EXCEEDED, ERR_SCHEMA_DIGEST_MISMATCH, ERR_SIDECAR_MALFORMED,
    ERR_SIDECAR_MISSING, ERR_SIDECAR_SCHEMA_INVALID, ERR_SIDECAR_SIZE_EXCEEDED,
    ERR_UNSUPPORTED_PROTOCOL, ErrorClass, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA, PROTOCOL_VERSION, SUPPORTED_METHODS,
    SUPPORTED_METHODS_V2, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult, ToolSchemaResult,
};
pub use resolver::DevModeResolver;
pub use runtime::{
    DEFAULT_PROCESS_COMMAND, ProcessRuntimeSpec, SandboxAdapterRuntimeSpec, SandboxImageRef,
    SandboxRuntimeSpec, ToolRuntime, ToolRuntimeKind,
};
pub use runtime_lock::{RUNTIME_LOCK_FILE_NAME, ToolRuntimeLock, read_runtime_lock};
pub use sandbox::canonical_bind_digest;
use serde_json::Value;
pub use session_handler::{ExternalSessionToolHandler, ExternalSessionToolSession};
pub use session_invoker::{
    SessionAbortRequest, SessionFinishRequest, SessionOpenRequest, SessionOpenResponse,
    SessionReadRequest, SessionReadResponse, SessionSendRequest, SessionToolInvoker,
};
pub use sidecar_bundle::{
    DEFAULT_SCHEMA_CONTENT_TYPE, SIDECAR_BUNDLE_ABS_PATH, SIDECAR_BUNDLE_REL_PATH, SIDECAR_DIR_ABS,
    ToolManifestSidecar, ToolRuntimeSidecar, ToolSchemaSidecar, ToolSidecarBundle,
    read_sidecar_bundle, render_sidecar_bundle,
};
pub use stdio::StdioSubprocessInvoker;

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
}

pub type ExternalLifecycleRecorder = Arc<dyn Fn(ExternalLifecycleEvent) + Send + Sync>;
