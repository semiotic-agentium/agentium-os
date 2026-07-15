// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Error types for the BAML runtime integration
//!
//! Provides a comprehensive error hierarchy using `thiserror` for proper error handling
//! and error chaining throughout the codebase.

use anyhow::Error as AnyhowError;
use thiserror::Error;

use crate::{
    semantics::{ErrorDisposition, Retryability},
    step_executor_outcome::StepPlanRecovery,
};

/// Structured tool failure for LLM-visible payloads and host retry policy.
///
/// Construct this from **typed** integration errors (`impl From<NotionError> for ClassifiedToolError`,
/// etc.) at the tool boundary. Do not rebuild this by parsing [`BamlRtError::ToolExecution`] strings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Error)]
#[error("{message}")]
pub struct ClassifiedToolError {
    /// Short machine-readable code (e.g. `notion_rate_limited`).
    pub code: String,
    pub disposition: ErrorDisposition,
    /// Message safe to show to the model (no secrets).
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl ClassifiedToolError {
    #[must_use]
    pub fn host_retryability(&self) -> Retryability {
        match self.disposition {
            ErrorDisposition::HostRetriable => Retryability::Retryable,
            _ => Retryability::Permanent,
        }
    }

    #[must_use]
    pub fn to_tool_error_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "code": self.code,
                "disposition": self.disposition,
                "message": self.message,
            })
        })
    }
}

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

    /// Agent instance is draining (undeploy in progress); new work must not be accepted.
    #[error("Agent draining: {0}")]
    AgentDraining(String),

    /// Invalid argument provided to a function
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    /// Tool session plan step violated a correctable contract (structured for `StepExecutorOutcome`).
    #[error(transparent)]
    StepPlanCorrectable(#[from] StepPlanRecovery),

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

    /// Semiotic tier-3 gate requires human authorization (A2A InputRequired).
    #[error("Gate authorization required: {prompt}")]
    GateAuthorizationRequired { prompt: String },

    /// Tool failed with structured classification from typed integration errors.
    #[error(transparent)]
    ToolClassified(#[from] ClassifiedToolError),

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
    #[error("Parsed result conversion failed: {source}")]
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

    /// Cluster heartbeat write to shared SurrealDB failed.
    #[error("Cluster heartbeat failed ({kind}): {message}")]
    ClusterHeartbeat {
        kind: HeartbeatErrorKind,
        message: String,
    },
}

/// Class of failure observed by the cluster heartbeat task.
///
/// Operator-visible on `GET /diagnose` as `cluster_heartbeat_last_error_kind`.
/// Connection failures point at SurrealDB transport (network / TLS / pool);
/// query failures point at server-side execution (timeout, schema, throttle);
/// permission failures point at credentials drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatErrorKind {
    /// SDK-side connection or transport failure.
    Connection,
    /// Server-side query execution failure (timeout, cancellation, etc.).
    Query,
    /// Permission or auth failure (token expired, role insufficient).
    NotAllowed,
    /// Anything else (validation, serialization, internal, unknown).
    Other,
}

impl HeartbeatErrorKind {
    #[must_use]
    pub fn as_code(&self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Query => "query",
            Self::NotAllowed => "not_allowed",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for HeartbeatErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_code())
    }
}

impl ClassifiedToolError {
    /// Classify from any [`BamlRtError`]. [`BamlRtError::ToolClassified`] is returned verbatim.
    #[must_use]
    pub fn from_baml_error(err: &BamlRtError) -> Self {
        if let BamlRtError::ToolClassified(c) = err {
            return c.clone();
        }
        let disposition = baml_error_disposition(err);
        let code = code_for_classified(err, disposition);
        let message = err.to_string();
        Self {
            code,
            disposition,
            message,
            hint: None,
            retry_after_ms: None,
        }
    }
}

fn code_for_classified(err: &BamlRtError, disposition: ErrorDisposition) -> String {
    match err {
        BamlRtError::InvalidArgument(_)
        | BamlRtError::InvalidArgumentWithSource { .. }
        | BamlRtError::InvalidOpenInput { .. }
        | BamlRtError::Json(_)
        | BamlRtError::JsonWithRaw { .. }
        | BamlRtError::FunctionNotFound(_)
        | BamlRtError::TypeConversion(_) => "invalid_argument".to_string(),
        BamlRtError::StepPlanCorrectable(r) => r.code.as_str().to_string(),
        BamlRtError::ToolExecution(_) => match disposition {
            ErrorDisposition::HostRetriable => "transient_tool_execution".to_string(),
            ErrorDisposition::LlmCorrectable
            | ErrorDisposition::InformAndContinue
            | ErrorDisposition::Fatal => "tool_execution".to_string(),
        },
        BamlRtError::ExecutionFailed { .. } | BamlRtError::ParsedResultFailed { .. } => {
            match disposition {
                ErrorDisposition::HostRetriable => "transient_execution".to_string(),
                ErrorDisposition::LlmCorrectable
                | ErrorDisposition::InformAndContinue
                | ErrorDisposition::Fatal => "execution_failed".to_string(),
            }
        }
        BamlRtError::Io(_) => "io_error".to_string(),
        BamlRtError::QuickJs(_) | BamlRtError::QuickJsWithSource { .. } => "quickjs".to_string(),
        BamlRtError::SessionLifecycle(_) => "session_lifecycle".to_string(),
        BamlRtError::Conflict(_) => "conflict".to_string(),
        _ => "runtime_error".to_string(),
    }
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
        BamlRtError::StepPlanCorrectable(r) => r.disposition,
        BamlRtError::AgentNotFound(_) => ErrorDisposition::InformAndContinue,
        BamlRtError::AgentDraining(_) => ErrorDisposition::HostRetriable,
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
        BamlRtError::ToolClassified(c) => c.disposition,
        BamlRtError::GateAuthorizationRequired { .. } => ErrorDisposition::InformAndContinue,
        BamlRtError::ToolExecution(_) => ErrorDisposition::InformAndContinue,
        BamlRtError::ExecutionFailed { .. } | BamlRtError::ParsedResultFailed { .. } => {
            ErrorDisposition::InformAndContinue
        }
        BamlRtError::Io(_)
        | BamlRtError::QuickJs(_)
        | BamlRtError::QuickJsWithSource { .. }
        | BamlRtError::ProvenanceContextRead { .. }
        | BamlRtError::ClusterHeartbeat { .. } => ErrorDisposition::HostRetriable,
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

/// Result type alias for convenience
pub type Result<T> = std::result::Result<T, BamlRtError>;
