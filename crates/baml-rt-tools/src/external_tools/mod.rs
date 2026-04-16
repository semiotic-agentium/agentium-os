//! External tool protocol + execution backend.
//!
//! This module defines the wire contract (JSON-RPC `tool/describe` + `tool/invoke`)
//! and the abstractions needed to run tools out-of-process over stdio/UDS.
//!
//! Phase 1 scope: stateless, single-shot subprocess invocation. Later phases
//! extend the same protocol to Wasm or keep-alive transports.

pub mod handler;
pub mod invoker;
pub mod policy;
pub mod protocol;
pub mod resolver;
pub mod stdio;

use std::sync::Arc;

pub use handler::{ProcessToolHandler, ProcessToolSession};
pub use invoker::{
    ExternalInvoker, InvokeRequest, InvokeResponse, ToolDescribe, map_jsonrpc_error,
};
pub use policy::{
    BACKOFF_SCHEDULE_MS, DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT, DEFAULT_MAX_CONCURRENT,
    DEFAULT_QUARANTINE_THRESHOLD, InvocationPolicy, PolicyError, QuarantineState, ToolQuota,
};
pub use protocol::{
    ErrorClass, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PROTOCOL_VERSION,
    ToolDescribeResult, ToolInvokeParams, ToolInvokeResult,
};
pub use resolver::DevModeResolver;
use serde_json::Value;
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
