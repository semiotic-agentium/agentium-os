// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON-RPC 2.0 wire contract for the sandbox tool protocol.
//!
//! Methods supported by the protocol:
//! - `tool/describe` — returns tool identity/capabilities.
//! - `tool/schema` — returns tool contract schema metadata + input/output schemas.
//! - `tool/invoke` — executes one invocation and returns a result (single-shot).
//!
//! Framing rules (stdio transport): see [`crate::codec`]. Frames are
//! length-prefixed JSON; stdout carries only framed JSON, stderr carries
//! free-form logs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version supported by this crate's default constants.
pub const PROTOCOL_VERSION: &str = "1";

/// Method name for the tool description handshake.
pub const METHOD_DESCRIBE: &str = "tool/describe";

/// Method name for tool schema introspection.
pub const METHOD_SCHEMA: &str = "tool/schema";

/// Method name for invoking the tool.
pub const METHOD_INVOKE: &str = "tool/invoke";

/// Method names a V1 tool is expected to declare in its `tool/describe`
/// response under `supported_methods`.
pub const SUPPORTED_METHODS: &[&str] = &[METHOD_DESCRIBE, METHOD_INVOKE];

/// Method names for protocol V2 tools that expose static schema via
/// `tool/schema`.
pub const SUPPORTED_METHODS_V2: &[&str] = &[METHOD_DESCRIBE, METHOD_SCHEMA, METHOD_INVOKE];

/// JSON-RPC 2.0 "Method not found" error code. Mirrors the spec constant.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// JSON-RPC 2.0 "Invalid params" error code. Mirrors the spec constant.
pub const ERR_INVALID_PARAMS: i32 = -32602;

/// JSON-RPC 2.0 "Parse error" error code. Used when the request is not valid JSON.
pub const ERR_PARSE_ERROR: i32 = -32700;

/// Application-defined server error code for generic tool execution failures.
/// Mirrors the lower bound of the JSON-RPC 2.0 reserved implementation-defined range.
pub const ERR_INTERNAL: i32 = -32000;

/// Sidecar bundle is missing at the required path.
pub const ERR_SIDECAR_MISSING: i32 = -32010;
/// Sidecar bundle bytes are present but malformed JSON/UTF-8.
pub const ERR_SIDECAR_MALFORMED: i32 = -32011;
/// Sidecar bundle shape/fields failed validation.
pub const ERR_SIDECAR_SCHEMA_INVALID: i32 = -32012;
/// Recomputed schema digest does not match declared content digest.
pub const ERR_SCHEMA_DIGEST_MISMATCH: i32 = -32013;
/// Sidecar/runtime protocol declaration is unsupported.
pub const ERR_UNSUPPORTED_PROTOCOL: i32 = -32014;
/// Static response payload exceeds configured size limit.
pub const ERR_PAYLOAD_LIMIT_EXCEEDED: i32 = -32015;
/// Sidecar file exceeds configured size limit.
pub const ERR_SIDECAR_SIZE_EXCEEDED: i32 = -32016;

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

/// JSON-RPC error object. `data.error_class` carries the machine-readable
/// [`ErrorClass`] value that host-side code maps to its own classified error
/// category.
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
    /// Digest of the schema content (`{input, output}`), if advertised by the tool.
    ///
    /// Accepts legacy `schema_hash` on decode for backward compatibility.
    #[serde(default, alias = "schema_hash")]
    pub schema_digest: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

/// Result payload returned from `tool/schema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchemaResult {
    pub schema_version: u64,
    pub tool_name: String,
    #[serde(default = "default_schema_content_type")]
    pub content_type: String,
    pub content_digest: String,
    pub input: Value,
    pub output: Value,
}

fn default_schema_content_type() -> String {
    "application/schema+json".to_string()
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
