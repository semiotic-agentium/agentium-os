//! Error types for the BAML runtime integration
//!
//! Provides a comprehensive error hierarchy using `thiserror` for proper error handling
//! and error chaining throughout the codebase.

use std::time::SystemTimeError;

use anyhow::Error as AnyhowError;
use thiserror::Error;

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

    /// Function execution failed
    #[error("Function execution failed")]
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
    #[error("Failed to build request: {0}")]
    RequestBuildFailed(String),

    /// Failed to load BAML runtime
    #[error("Failed to load BAML runtime")]
    RuntimeLoadFailed {
        #[source]
        source: AnyhowError,
    },

    /// System time error
    #[error("System time error: {0}")]
    SystemTime(#[from] SystemTimeError),

    /// Tar header path error
    #[error("Failed to set tar header path")]
    TarHeaderPath(#[source] std::io::Error),
}

/// Result type alias for convenience
pub type Result<T> = std::result::Result<T, BamlRtError>;
