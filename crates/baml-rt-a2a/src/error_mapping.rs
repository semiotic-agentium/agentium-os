//! Canonical mapping from BamlRtError to A2A JSON-RPC error and classifier string.
//! Single source of truth for both response formatting and error classification.
//! I4: every mapped error includes retryable (transient vs permanent) and classifier.

use baml_rt_core::BamlRtError;
use serde_json::Value;

/// Result of mapping a BamlRtError to A2A error representation.
pub struct A2aErrorMapping {
    pub code: i64,
    pub message: &'static str,
    pub data: Option<Value>,
    pub classifier: &'static str,
    /// I4: machine-readable retryability for clients (transient vs permanent).
    pub retryable: bool,
}

fn data_with_retryability(
    error: &BamlRtError,
    base_data: Option<Value>,
    classifier: &'static str,
    retryable: bool,
) -> Option<Value> {
    let mut map = match base_data {
        Some(Value::Object(m)) => m,
        Some(other) => return Some(other),
        None => serde_json::Map::new(),
    };
    map.insert("error".to_string(), Value::String(error.to_string()));
    map.insert(
        "classifier".to_string(),
        Value::String(classifier.to_string()),
    );
    map.insert("retryable".to_string(), Value::Bool(retryable));
    Some(Value::Object(map))
}

fn mapping(
    error: &BamlRtError,
    code: i64,
    message: &'static str,
    data: Option<Value>,
    classifier: &'static str,
    retryable: bool,
) -> A2aErrorMapping {
    A2aErrorMapping {
        code,
        message,
        data: data_with_retryability(error, data, classifier, retryable),
        classifier,
        retryable,
    }
}

/// Maps a BamlRtError to code, message, optional data, classifier, and retryable.
pub fn map_error(error: &BamlRtError) -> A2aErrorMapping {
    match error {
        BamlRtError::InvalidArgument(message) => mapping(
            error,
            -32600,
            "Invalid request",
            Some(serde_json::json!({ "details": message })),
            "invalid_argument",
            false,
        ),
        BamlRtError::InvalidArgumentWithSource { .. } => mapping(
            error,
            -32600,
            "Invalid request",
            None,
            "invalid_argument",
            false,
        ),
        BamlRtError::FunctionNotFound(name) => mapping(
            error,
            -32601,
            "Method not found",
            Some(serde_json::json!({ "function": name })),
            "function_not_found",
            false,
        ),
        BamlRtError::Json(json_err) => mapping(
            error,
            -32700,
            "Parse error",
            Some(serde_json::json!({ "details": json_err.to_string() })),
            "json",
            false,
        ),
        BamlRtError::JsonWithRaw { .. } => {
            mapping(error, -32700, "Parse error", None, "json", false)
        }
        BamlRtError::QuickJsWithSource { context, .. } => mapping(
            error,
            -32603,
            "Internal error",
            Some(serde_json::json!({ "context": context })),
            "quickjs",
            true,
        ),
        BamlRtError::QuickJs(_) => mapping(error, -32603, "Internal error", None, "quickjs", true),
        BamlRtError::ToolExecution(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "tool_execution",
            true,
        ),
        BamlRtError::ProvenanceContextRead { .. } => {
            mapping(error, -32603, "Internal error", None, "provenance", true)
        }
        BamlRtError::Io(_) => mapping(error, -32603, "Internal error", None, "io", true),
        BamlRtError::ExecutionFailed { .. } => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "execution_failed",
            true,
        ),
        BamlRtError::RequestBuildFailed(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "request_build_failed",
            true,
        ),
        BamlRtError::InvalidOpenInput { .. } => mapping(
            error,
            -32600,
            "Invalid request",
            None,
            "invalid_open_input",
            false,
        ),
        BamlRtError::ToolRegistration(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "tool_registration",
            false,
        ),
        BamlRtError::SchemaLoading(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "schema_loading",
            false,
        ),
        BamlRtError::Configuration(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "configuration",
            false,
        ),
        BamlRtError::Initialization(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "initialization",
            false,
        ),
        BamlRtError::RuntimeLoadFailed { .. } => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "runtime_load_failed",
            true,
        ),
        BamlRtError::BamlRuntime(_) | BamlRtError::TypeConversion(_) => {
            mapping(error, -32603, "Internal error", None, "internal", false)
        }
        BamlRtError::ParsedResultFailed { .. }
        | BamlRtError::SystemTime(_)
        | BamlRtError::TarHeaderPath(_) => {
            mapping(error, -32603, "Internal error", None, "internal", true)
        }
    }
}
