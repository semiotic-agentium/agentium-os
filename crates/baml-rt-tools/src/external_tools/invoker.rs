//! `ExternalInvoker` — transport-abstract interface for the tool protocol.
//!
//! V1 default transport is a stateless subprocess speaking JSON-RPC over stdio
//! ([`super::stdio::StdioSubprocessInvoker`]). Later backends (UDS, Wasm) implement
//! the same trait so callers don't depend on transport details.

use std::time::Duration;

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, ClassifiedToolError, ErrorDisposition, Result};
use serde_json::Value;

use crate::ToolName;

use super::protocol::{ErrorClass, JsonRpcError, ToolDescribeResult};

/// Runner-side request for `tool/invoke`.
#[derive(Debug, Clone)]
pub struct InvokeRequest {
    pub tool_name: ToolName,
    pub invocation_id: String,
    pub input: Value,
    pub secrets: serde_json::Map<String, Value>,
    pub capabilities: Value,
    pub timeout: Duration,
}

/// Runner-side response for `tool/invoke`.
#[derive(Debug, Clone)]
pub struct InvokeResponse {
    pub output: Value,
    pub done: bool,
}

/// Runner-side view of the `tool/describe` reply, flattened for ergonomic use.
#[derive(Debug, Clone)]
pub struct ToolDescribe {
    pub protocol_version: String,
    pub tool_name: String,
    pub supported_methods: Vec<String>,
    pub max_payload_bytes: Option<u64>,
    pub schema_hash: Option<String>,
}

impl From<ToolDescribeResult> for ToolDescribe {
    fn from(r: ToolDescribeResult) -> Self {
        Self {
            protocol_version: r.protocol_version,
            tool_name: r.tool_name,
            supported_methods: r.supported_methods,
            max_payload_bytes: r.max_payload_bytes,
            schema_hash: r.schema_hash,
        }
    }
}

/// Transport-abstract interface for the external tool protocol.
#[async_trait]
pub trait ExternalInvoker: Send + Sync {
    /// Validate the tool's protocol contract. Should be called once per
    /// `(tool_name, digest)` at load time and cached.
    async fn describe(&self, tool: &ToolName, timeout: Duration) -> Result<ToolDescribe>;

    /// Execute a single `tool/invoke`. Stateless: one call, one result.
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse>;
}

/// Map a JSON-RPC error to a [`BamlRtError`] with preserved classification.
///
/// Reads `error.data.error_class` when present; missing/unknown classes default
/// to [`ErrorClass::Execution`] (safe fallback).
pub fn map_jsonrpc_error(tool: &ToolName, err: &JsonRpcError) -> BamlRtError {
    let class = err
        .data
        .as_ref()
        .and_then(|d| d.get("error_class"))
        .and_then(|c| serde_json::from_value::<ErrorClass>(c.clone()).ok())
        .unwrap_or_default();

    let disposition = match class {
        ErrorClass::Configuration => ErrorDisposition::Fatal,
        ErrorClass::InvalidArgument => ErrorDisposition::LlmCorrectable,
        ErrorClass::Transient => ErrorDisposition::HostRetriable,
        ErrorClass::Permission => ErrorDisposition::Fatal,
        ErrorClass::Execution => ErrorDisposition::InformAndContinue,
    };

    let code = format!("external_{}_{}", tool, match class {
        ErrorClass::Configuration => "configuration",
        ErrorClass::InvalidArgument => "invalid_argument",
        ErrorClass::Transient => "transient",
        ErrorClass::Permission => "permission",
        ErrorClass::Execution => "execution",
    });

    let retry_after_ms = err
        .data
        .as_ref()
        .and_then(|d| d.get("retry_after_ms"))
        .and_then(|v| v.as_u64());

    let classified = ClassifiedToolError {
        code,
        disposition,
        message: err.message.clone(),
        hint: None,
        retry_after_ms,
    };

    BamlRtError::ToolClassified(classified)
}
