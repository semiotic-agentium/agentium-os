//! Error types for the BAML runtime integration
//!
//! Provides a comprehensive error hierarchy using `thiserror` for proper error handling
//! and error chaining throughout the codebase.

use anyhow::Error as AnyhowError;
use thiserror::Error;

use crate::semantics::{ErrorDisposition, Retryability};

/// Typed lifecycle failures for stream/tool sessions.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SessionLifecycleError {
    #[error("Tool session not found: {session_id}")]
    ToolSessionNotFound { session_id: String },

    #[error("Tool session closed: {session_id}")]
    ToolSessionClosed { session_id: String },

    #[error("Stream session not found: {stream_session_id}")]
    StreamSessionNotFound { stream_session_id: u64 },

    #[error("Stream session closed: {stream_session_id}")]
    StreamSessionClosed { stream_session_id: u64 },

    #[error("Invocation context missing")]
    InvocationContextMissing,

    #[error("Invocation cancelled")]
    InvocationCancelled,
}

impl SessionLifecycleError {
    /// Stable error code for metrics/log labels and bridge mappings.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ToolSessionNotFound { .. } => "tool_session_not_found",
            Self::ToolSessionClosed { .. } => "tool_session_closed",
            Self::StreamSessionNotFound { .. } => "stream_session_not_found",
            Self::StreamSessionClosed { .. } => "stream_session_closed",
            Self::InvocationContextMissing => "invocation_context_missing",
            Self::InvocationCancelled => "invocation_cancelled",
        }
    }
}

/// Main error type for the BAML runtime integration
#[derive(Error, Debug)]
pub enum BamlRtError {
    /// BAML runtime execution error
    #[error("BAML runtime error: {0}")]
    BamlRuntime(String),

    /// Failed to read provenance context (e.g. conversation history).
    #[error("Failed to read provenance context")]
    ProvenanceContextRead {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// QuickJS JavaScript engine error
    #[error("QuickJS error: {0}")]
    QuickJs(String),

    /// QuickJS error with source
    #[error("QuickJS error: {context}")]
    QuickJsWithSource {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Type conversion error between Rust and JavaScript types
    #[error("Type conversion error: {0}")]
    TypeConversion(String),

    /// Function not found in registry
    #[error("Function not found: {0}")]
    FunctionNotFound(String),

    /// Requested agent was not found in the registry.
    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    /// Invalid argument provided to a function
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    /// Request conflicts with current runtime/session state
    #[error("Conflict: {0}")]
    Conflict(String),

    /// Invalid argument with source error
    #[error("{message}")]
    InvalidArgumentWithSource {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// I/O error (file operations, etc.)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// JSON parse error with raw input for diagnostics (e.g. evaluate promise result)
    #[error("JSON error: {source} (raw length: {raw_length}, prefix: {raw_prefix:?})")]
    JsonWithRaw {
        #[source]
        source: serde_json::Error,
        raw_length: usize,
        raw_prefix: String,
    },

    /// Invalid open_input for tool session
    #[error("Invalid open_input for tool session")]
    InvalidOpenInput {
        #[source]
        source: serde_json::Error,
    },

    /// Tool execution error
    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    /// Tool registration error
    #[error("Tool registration error: {0}")]
    ToolRegistration(String),

    /// Schema loading error
    #[error("Schema loading error: {0}")]
    SchemaLoading(String),

    /// Runtime configuration error
    #[error("Runtime configuration error: {0}")]
    Configuration(String),

    /// Runtime initialization error
    #[error("Runtime initialization error: {0}")]
    Initialization(String),

    /// Stream/tool session lifecycle failure.
    #[error("Session lifecycle error: {0}")]
    SessionLifecycle(#[from] SessionLifecycleError),

    /// BAML `call_function` / step-executor hop failed (cause is the wrapped anyhow chain).
    #[error("Function execution failed: {source}")]
    ExecutionFailed {
        #[source]
        source: AnyhowError,
    },

    /// Parsed result conversion failed
    #[error("Parsed result conversion failed")]
    ParsedResultFailed {
        #[source]
        source: AnyhowError,
    },

    /// Failed to build request
    #[error("Failed to build request")]
    RequestBuildFailed {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Failed to build LLM client registry (e.g. from secret resolver + IR)
    #[error("Failed to build LLM client registry")]
    ClientRegistryBuild {
        #[source]
        source: AnyhowError,
    },

    /// Failed to load BAML runtime
    #[error("Failed to load BAML runtime")]
    RuntimeLoadFailed {
        #[source]
        source: AnyhowError,
    },

    /// Failed to create BAML function result stream
    #[error("Failed to create stream")]
    FunctionStreamCreation {
        #[source]
        source: AnyhowError,
    },
}

/// Classify how this error should be surfaced for host retries vs LLM correction.
pub fn baml_error_disposition(err: &BamlRtError) -> ErrorDisposition {
    match err {
        BamlRtError::InvalidArgument(_)
        | BamlRtError::InvalidArgumentWithSource { .. }
        | BamlRtError::InvalidOpenInput { .. }
        | BamlRtError::Json(_)
        | BamlRtError::JsonWithRaw { .. }
        | BamlRtError::FunctionNotFound(_)
        | BamlRtError::TypeConversion(_) => ErrorDisposition::LlmCorrectable,
        BamlRtError::AgentNotFound(_) => ErrorDisposition::InformAndContinue,
        BamlRtError::Conflict(_) => ErrorDisposition::HostRetriable,
        BamlRtError::SessionLifecycle(
            SessionLifecycleError::ToolSessionNotFound { .. }
            | SessionLifecycleError::ToolSessionClosed { .. },
        ) => ErrorDisposition::InformAndContinue,
        BamlRtError::SessionLifecycle(
            SessionLifecycleError::StreamSessionNotFound { .. }
            | SessionLifecycleError::StreamSessionClosed { .. }
            | SessionLifecycleError::InvocationContextMissing,
        ) => ErrorDisposition::InformAndContinue,
        BamlRtError::SessionLifecycle(SessionLifecycleError::InvocationCancelled) => {
            ErrorDisposition::HostRetriable
        }
        BamlRtError::ToolExecution(msg) => disposition_from_tool_execution_message(msg),
        BamlRtError::ExecutionFailed { source } => disposition_from_anyhow_chain(source),
        BamlRtError::ParsedResultFailed { source } => disposition_from_anyhow_chain(source),
        BamlRtError::Io(_)
        | BamlRtError::QuickJs(_)
        | BamlRtError::QuickJsWithSource { .. }
        | BamlRtError::ProvenanceContextRead { .. } => ErrorDisposition::HostRetriable,
        BamlRtError::RequestBuildFailed { .. } => ErrorDisposition::LlmCorrectable,
        BamlRtError::ClientRegistryBuild { .. }
        | BamlRtError::RuntimeLoadFailed { .. }
        | BamlRtError::FunctionStreamCreation { .. }
        | BamlRtError::BamlRuntime(_)
        | BamlRtError::Initialization(_)
        | BamlRtError::Configuration(_)
        | BamlRtError::SchemaLoading(_)
        | BamlRtError::ToolRegistration(_) => ErrorDisposition::Fatal,
    }
}

/// JSON-RPC / client retry hint derived from [`baml_error_disposition`].
pub fn retryability_for_a2a(err: &BamlRtError) -> Retryability {
    match baml_error_disposition(err) {
        ErrorDisposition::HostRetriable => Retryability::Retryable,
        ErrorDisposition::LlmCorrectable
        | ErrorDisposition::InformAndContinue
        | ErrorDisposition::Fatal => Retryability::Permanent,
    }
}

fn disposition_from_tool_execution_message(msg: &str) -> ErrorDisposition {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("rate limit")
        || lower.contains("rate_limited")
        || lower.contains("429")
        || lower.contains("timeout")
        || lower.contains("temporarily unavailable")
        || lower.contains("503")
        || lower.contains("connection reset")
    {
        return ErrorDisposition::HostRetriable;
    }
    if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("403")
        || lower.contains("forbidden")
        || lower.contains("not found")
        || lower.contains("404")
        || lower.contains("invalid_argument")
    {
        return ErrorDisposition::InformAndContinue;
    }
    ErrorDisposition::InformAndContinue
}

fn disposition_from_anyhow_chain(source: &AnyhowError) -> ErrorDisposition {
    let chain = format!("{source:#}");
    let lower = chain.to_ascii_lowercase();
    if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("504")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
    {
        return ErrorDisposition::HostRetriable;
    }
    if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("403")
        || lower.contains("forbidden")
        || lower.contains("404")
        || lower.contains("not found")
    {
        return ErrorDisposition::InformAndContinue;
    }
    if lower.contains("invalid")
        || lower.contains("argument")
        || lower.contains("schema")
        || lower.contains("parse")
    {
        return ErrorDisposition::LlmCorrectable;
    }
    ErrorDisposition::Fatal
}

/// Result type alias for convenience
pub type Result<T> = std::result::Result<T, BamlRtError>;
