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

pub use handler::{ProcessToolHandler, ProcessToolSession};
pub use resolver::DevModeResolver;
pub use invoker::{ExternalInvoker, InvokeRequest, InvokeResponse, ToolDescribe, map_jsonrpc_error};
pub use policy::{
    BACKOFF_SCHEDULE_MS, DEFAULT_DESCRIBE_TIMEOUT, DEFAULT_INVOKE_TIMEOUT, DEFAULT_MAX_CONCURRENT,
    DEFAULT_QUARANTINE_THRESHOLD, InvocationPolicy, PolicyError, QuarantineState, ToolQuota,
};
pub use protocol::{
    ErrorClass, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PROTOCOL_VERSION,
    ToolDescribeResult, ToolInvokeParams, ToolInvokeResult,
};
pub use stdio::StdioSubprocessInvoker;
