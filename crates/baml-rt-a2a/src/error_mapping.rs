//! Canonical mapping from BamlRtError to A2A JSON-RPC error and classifier string.
//! Single source of truth for both response formatting and error classification.
//! I4: every mapped error includes retryable (transient vs permanent) and classifier.

use baml_rt_core::{BamlRtError, Retryability};
use serde_json::Value;

/// Result of mapping a BamlRtError to A2A error representation.
pub struct A2aErrorMapping {
    pub code: i64,
    pub message: &'static str,
    pub data: Option<Value>,
    pub classifier: &'static str,
    /// I4: machine-readable retryability for clients (transient vs permanent).
    pub retryable: Retryability,
}

fn data_with_retryability(
    error: &BamlRtError,
    base_data: Option<Value>,
    classifier: &'static str,
    retryable: Retryability,
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
    map.insert(
        "retryable".to_string(),
        Value::Bool(retryable.is_retryable()),
    );
    Some(Value::Object(map))
}

fn mapping(
    error: &BamlRtError,
    code: i64,
    message: &'static str,
    data: Option<Value>,
    classifier: &'static str,
    retryable: Retryability,
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
        BamlRtError::AgentNotFound(message) => mapping(
            error,
            -32601,
            "Agent not found",
            Some(serde_json::json!({ "details": message })),
            "agent_not_found",
            Retryability::Permanent,
        ),
        BamlRtError::InvalidArgument(message) => mapping(
            error,
            -32600,
            "Invalid request",
            Some(serde_json::json!({ "details": message })),
            "invalid_argument",
            Retryability::Permanent,
        ),
        BamlRtError::Conflict(message) => mapping(
            error,
            -32009,
            "Conflict",
            Some(serde_json::json!({ "details": message })),
            "conflict",
            Retryability::Retryable,
        ),
        BamlRtError::InvalidArgumentWithSource { .. } => mapping(
            error,
            -32600,
            "Invalid request",
            None,
            "invalid_argument",
            Retryability::Permanent,
        ),
        BamlRtError::SessionLifecycle(lifecycle) => {
            let (code, message, classifier, retryable) = match lifecycle {
                baml_rt_core::SessionLifecycleError::InvocationCancelled => (
                    -32603,
                    "Internal error",
                    "invocation_cancelled",
                    Retryability::Retryable,
                ),
                _ => (
                    -32600,
                    "Invalid request",
                    "session_lifecycle",
                    Retryability::Permanent,
                ),
            };
            mapping(error, code, message, None, classifier, retryable)
        }
        BamlRtError::FunctionNotFound(name) => mapping(
            error,
            -32601,
            "Method not found",
            Some(serde_json::json!({ "function": name })),
            "function_not_found",
            Retryability::Permanent,
        ),
        BamlRtError::Json(json_err) => mapping(
            error,
            -32700,
            "Parse error",
            Some(serde_json::json!({ "details": json_err.to_string() })),
            "json",
            Retryability::Permanent,
        ),
        BamlRtError::JsonWithRaw { .. } => mapping(
            error,
            -32700,
            "Parse error",
            None,
            "json",
            Retryability::Permanent,
        ),
        BamlRtError::QuickJsWithSource { context, .. } => mapping(
            error,
            -32603,
            "Internal error",
            Some(serde_json::json!({ "context": context })),
            "quickjs",
            Retryability::Retryable,
        ),
        BamlRtError::QuickJs(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "quickjs",
            Retryability::Retryable,
        ),
        BamlRtError::ToolExecution(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "tool_execution",
            Retryability::Retryable,
        ),
        BamlRtError::ProvenanceContextRead { source } => {
            let details = format!("{source}");
            mapping(
                error,
                -32603,
                "Internal error",
                Some(serde_json::json!({ "details": details })),
                "provenance",
                Retryability::Retryable,
            )
        }
        BamlRtError::Io(io_err) => mapping(
            error,
            -32603,
            "Internal error",
            Some(serde_json::json!({ "details": io_err.to_string(), "layer": "io" })),
            "io",
            Retryability::Retryable,
        ),
        BamlRtError::ExecutionFailed { source } => mapping(
            error,
            -32603,
            "Internal error",
            Some(serde_json::json!({ "details": source.to_string(), "layer": "execution" })),
            "execution_failed",
            Retryability::Retryable,
        ),
        BamlRtError::RequestBuildFailed { source } => mapping(
            error,
            -32603,
            "Internal error",
            Some(serde_json::json!({ "details": source.to_string(), "layer": "request_build" })),
            "request_build_failed",
            Retryability::Retryable,
        ),
        BamlRtError::InvalidOpenInput { .. } => mapping(
            error,
            -32600,
            "Invalid request",
            None,
            "invalid_open_input",
            Retryability::Permanent,
        ),
        BamlRtError::ToolRegistration(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "tool_registration",
            Retryability::Permanent,
        ),
        BamlRtError::SchemaLoading(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "schema_loading",
            Retryability::Permanent,
        ),
        BamlRtError::Configuration(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "configuration",
            Retryability::Permanent,
        ),
        BamlRtError::Initialization(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "initialization",
            Retryability::Permanent,
        ),
        BamlRtError::RuntimeLoadFailed { .. } => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "runtime_load_failed",
            Retryability::Retryable,
        ),
        BamlRtError::ClientRegistryBuild { .. } => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "client_registry_build",
            Retryability::Retryable,
        ),
        BamlRtError::FunctionStreamCreation { .. } => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "function_stream_creation",
            Retryability::Retryable,
        ),
        BamlRtError::BamlRuntime(_) | BamlRtError::TypeConversion(_) => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "internal",
            Retryability::Permanent,
        ),
        BamlRtError::ParsedResultFailed { .. } => mapping(
            error,
            -32603,
            "Internal error",
            None,
            "internal",
            Retryability::Retryable,
        ),
    }
}
