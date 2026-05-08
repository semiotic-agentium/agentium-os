//! JSON-RPC 2.0 wire contract for the external tool protocol.
//!
//! The authoritative definitions live in the [`baml_sandbox_protocol`] crate
//! so host and guest adapters share a single source of truth. This module
//! re-exports them under the historical path
//! `baml_rt_tools::external_tools::protocol::*` so existing call sites keep
//! compiling unchanged.

pub use baml_sandbox_protocol::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_PARSE_ERROR,
    ERR_PAYLOAD_LIMIT_EXCEEDED, ERR_SCHEMA_DIGEST_MISMATCH, ERR_SIDECAR_MALFORMED,
    ERR_SIDECAR_MISSING, ERR_SIDECAR_SCHEMA_INVALID, ERR_SIDECAR_SIZE_EXCEEDED,
    ERR_UNSUPPORTED_PROTOCOL, ErrorClass, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    METHOD_DESCRIBE, METHOD_INVOKE, METHOD_SCHEMA, PROTOCOL_VERSION, SUPPORTED_METHODS,
    SUPPORTED_METHODS_V2, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult, ToolSchemaResult,
};
