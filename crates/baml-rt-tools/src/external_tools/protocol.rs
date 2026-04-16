//! JSON-RPC 2.0 wire contract for the external tool protocol.
//!
//! Two methods are supported in V1:
//! - `tool/describe` — returns the tool's ABI, protocol version, and schema hash.
//! - `tool/invoke` — executes one invocation and returns a result (single-shot).
//!
//! Framing rules (stdio transport):
//! - stdout: one JSON-RPC frame per line, nothing else.
//! - stderr: free-form logs/diagnostics.
//! - non-JSON data on stdout is a protocol error.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version supported by V1.
pub const PROTOCOL_VERSION: &str = "1";

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    pub params: Value,
}

impl JsonRpcRequest {
    pub fn new(method: impl Into<String>, id: u64, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            id,
            params,
        }
    }
}

/// JSON-RPC 2.0 response envelope. Either `result` or `error` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object. `data.error_class` carries the machine-readable class
/// that maps to [`crate::ClassifiedToolError`] categories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Machine-readable error class carried in `error.data.error_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Tool misconfigured (e.g. missing/invalid API key).
    Configuration,
    /// Caller-supplied input was invalid (LLM-correctable).
    InvalidArgument,
    /// Transient/retriable failure (e.g. 5xx, rate limit).
    Transient,
    /// Authorization/permission denied.
    Permission,
    /// Generic execution failure (default when class is absent/unknown).
    #[default]
    Execution,
}

/// Result payload returned from `tool/describe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescribeResult {
    pub protocol_version: String,
    pub tool_name: String,
    #[serde(default)]
    pub supported_methods: Vec<String>,
    #[serde(default)]
    pub max_payload_bytes: Option<u64>,
    #[serde(default)]
    pub schema_hash: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

/// Params for `tool/invoke`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvokeParams {
    pub invocation_id: String,
    pub tool_name: String,
    pub input: Value,
    /// Secrets resolved by the runner for this invocation only.
    /// Never persisted/cached across calls.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub secrets: serde_json::Map<String, Value>,
    /// Capability subset effective for this invocation (policy ∩ tool declaration).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub capabilities: Value,
}

/// Result payload returned from `tool/invoke`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvokeResult {
    pub output: Value,
    #[serde(default)]
    pub done: bool,
}
