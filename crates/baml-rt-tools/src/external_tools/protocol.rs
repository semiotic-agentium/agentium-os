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

/// Method name for the tool description handshake.
pub const METHOD_DESCRIBE: &str = "tool/describe";

/// Method name for invoking the tool.
pub const METHOD_INVOKE: &str = "tool/invoke";

/// Method names a V1 tool is expected to declare in its `tool/describe`
/// response under `supported_methods`. Scaffolders render this list into the
/// generated handler so CLI output and runtime contract stay aligned.
///
/// `tool/describe` itself is not in this list — it is the handshake that
/// produces the list, not a member of it.
pub const SUPPORTED_METHODS: &[&str] = &[METHOD_INVOKE];

/// JSON-RPC 2.0 "Method not found" error code. Mirrors the spec constant.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// JSON-RPC 2.0 "Parse error" error code. Used when the request is not valid JSON.
pub const ERR_PARSE_ERROR: i32 = -32700;

/// Application-defined server error code for generic tool execution failures.
/// Mirrors the lower bound of the JSON-RPC 2.0 reserved implementation-defined range.
pub const ERR_INTERNAL: i32 = -32000;

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
