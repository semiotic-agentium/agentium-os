//! JSON-RPC 2.0 wire contract for the external tool protocol.
//!
//! The authoritative definitions live in the [`baml_sandbox_protocol`] crate
//! so host and guest adapters share a single source of truth. This module
//! re-exports them under the historical path
//! `baml_rt_tools::external_tools::protocol::*` so existing call sites keep
//! compiling unchanged.

pub use baml_sandbox_protocol::{
    ERR_INTERNAL, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR, ErrorClass, JsonRpcError, JsonRpcRequest,
    JsonRpcResponse, METHOD_DESCRIBE, METHOD_INVOKE, PROTOCOL_VERSION, SUPPORTED_METHODS,
    ToolDescribeResult, ToolInvokeParams, ToolInvokeResult,
};
