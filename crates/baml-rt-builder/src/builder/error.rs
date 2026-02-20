//! Error types for the BAML agent builder.
//!
//! `BamlBuilderError` captures all failure modes specific to the builder pipeline
//! (compilation, linting, packaging, bootstrap, codegen). Runtime errors from
//! `baml-rt-core` propagate through the `Runtime` variant via `#[from]`.

use std::path::PathBuf;

use thiserror::Error;

/// Builder-specific error type.
///
/// Keeps `BamlRtError` focused on runtime concerns by owning all
/// builder/packaging/codegen failure modes here.
#[derive(Error, Debug)]
pub enum BamlBuilderError {
    // ── Moved from BamlRtError (builder-only) ──────────────────────────
    /// Agent directory does not exist
    #[error("Agent directory does not exist: {}", .path.display())]
    AgentDirNotFound { path: PathBuf },

    /// Required `baml_src` subdirectory not found in agent directory
    #[error("baml_src directory not found in {}", .path.display())]
    BamlSrcNotFound { path: PathBuf },

    /// Package file does not exist
    #[error("Package file does not exist: {}", .path.display())]
    PackageNotFound { path: PathBuf },

    /// Package file has an invalid extension (expected .tar.gz)
    #[error("Package file must have .tar.gz extension: {}", .path.display())]
    InvalidPackageExtension { path: PathBuf },

    /// System time error (e.g. clock before UNIX epoch during build-dir creation)
    #[error("System time error")]
    SystemTime(#[source] std::time::SystemTimeError),

    /// Failed to set tar header path
    #[error("Failed to set tar header path")]
    TarHeaderPath(#[source] std::io::Error),

    /// Failed to extract BAML IR signatures
    #[error("Failed to extract BAML IR signatures")]
    IrSignatureExtraction {
        #[source]
        source: anyhow::Error,
    },

    // ── Builder-local convenience variants ─────────────────────────────
    /// I/O error (file operations)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[source] serde_json::Error),

    /// Invalid argument (catch-all for validation failures)
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    /// Invalid argument with a preserved source error
    #[error("{message}")]
    InvalidArgumentWithSource {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Failed to load BAML runtime
    #[error("Failed to load BAML runtime")]
    RuntimeLoadFailed {
        #[source]
        source: anyhow::Error,
    },

    // ── Pass-through for runtime library errors ────────────────────────
    /// Any `BamlRtError` that propagates through builder code via `?`.
    #[error(transparent)]
    Runtime(#[from] baml_rt_core::BamlRtError),
}

/// Result type alias for builder operations.
pub type Result<T> = std::result::Result<T, BamlBuilderError>;

/// Append a line to `output`, mapping `fmt::Error` into `BamlBuilderError`.
pub fn write_line(output: &mut String, line: &str) -> Result<()> {
    use std::fmt::Write;
    writeln!(output, "{}", line).map_err(|e| BamlBuilderError::InvalidArgumentWithSource {
        message: "Format error".into(),
        source: Box::new(e),
    })
}
