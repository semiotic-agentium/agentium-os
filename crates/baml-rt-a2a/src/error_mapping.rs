//! Canonical mapping from BamlRtError to A2A JSON-RPC error and classifier string.
//! Single source of truth for both response formatting and error classification.

use baml_rt_core::BamlRtError;
use serde_json::Value;

/// Result of mapping a BamlRtError to A2A error representation.
pub struct A2aErrorMapping {
    pub code: i64,
    pub message: &'static str,
    pub data: Option<Value>,
    pub classifier: &'static str,
}

/// Maps a BamlRtError to code, message, optional data, and classifier string.
pub fn map_error(error: &BamlRtError) -> A2aErrorMapping {
    match error {
        BamlRtError::InvalidArgument(message) => A2aErrorMapping {
            code: -32600,
            message: "Invalid request",
            data: Some(serde_json::json!({
                "error": error.to_string(),
                "details": message,
            })),
            classifier: "invalid_argument",
        },
        BamlRtError::FunctionNotFound(name) => A2aErrorMapping {
            code: -32601,
            message: "Method not found",
            data: Some(serde_json::json!({
                "error": error.to_string(),
                "function": name,
            })),
            classifier: "function_not_found",
        },
        BamlRtError::Json(json_err) => A2aErrorMapping {
            code: -32700,
            message: "Parse error",
            data: Some(serde_json::json!({
                "error": error.to_string(),
                "details": json_err.to_string(),
            })),
            classifier: "json",
        },
        BamlRtError::QuickJsWithSource { context, .. } => A2aErrorMapping {
            code: -32603,
            message: "Internal error",
            data: Some(serde_json::json!({
                "error": error.to_string(),
                "context": context,
            })),
            classifier: "quickjs",
        },
        BamlRtError::QuickJs(_) => A2aErrorMapping {
            code: -32603,
            message: "Internal error",
            data: Some(serde_json::json!({
                "error": error.to_string(),
            })),
            classifier: "quickjs",
        },
        BamlRtError::ToolExecution(_) => A2aErrorMapping {
            code: -32603,
            message: "Internal error",
            data: Some(serde_json::json!({
                "error": error.to_string(),
            })),
            classifier: "tool_execution",
        },
        BamlRtError::ProvenanceContextRead { .. } => A2aErrorMapping {
            code: -32603,
            message: "Internal error",
            data: Some(serde_json::json!({
                "error": error.to_string(),
            })),
            classifier: "provenance",
        },
        _ => A2aErrorMapping {
            code: -32603,
            message: "Internal error",
            data: Some(serde_json::json!({
                "error": error.to_string(),
            })),
            classifier: "internal",
        },
    }
}
