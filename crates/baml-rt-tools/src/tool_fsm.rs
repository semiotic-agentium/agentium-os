//! Tool session FSM primitives.

use std::{borrow::Cow, fmt};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Retryability};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::tool_error_classify::{ClassifiedToolError, ToolExecutionClassifier};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolSessionId(Uuid);

impl ToolSessionId {
    /// Create a new session ID from a UUID
    pub fn new(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Generate a random session ID
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse a session ID from a string UUID
    pub fn parse(id: impl Into<String>) -> std::result::Result<Self, BamlRtError> {
        let value = id.into();
        let uuid = Uuid::parse_str(&value).map_err(|e| {
            BamlRtError::InvalidArgument(format!("Invalid tool session id '{}': {}", value, e))
        })?;
        Ok(Self(uuid))
    }

    /// Get the UUID value
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Get the string representation as a `Cow`.
    ///
    /// Since UUIDs require formatting, this always returns `Cow::Owned`.
    /// For cases where you need a `&str`, use `.as_ref()` on the result.
    /// For an owned `String`, use `.into_owned()` or the `Display` trait's `to_string()`.
    pub fn as_str(&self) -> Cow<'static, str> {
        // UUID formatting always requires allocation, so return Owned
        // Callers can convert to &str via .as_ref() if needed
        Cow::Owned(self.0.to_string())
    }
}

impl fmt::Display for ToolSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolFailureKind {
    InvalidInput,
    ExecutionFailed,
    NotAuthorized,
    RateLimited,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub kind: ToolFailureKind,
    pub message: String,
    pub retryability: Retryability,
    /// Structured classification for LLM-visible payloads and host retry policy.
    pub classified: ClassifiedToolError,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    #[default]
    Open,
    Closed,
}

impl SessionPhase {
    pub const fn is_closed(self) -> bool {
        matches!(self, Self::Closed)
    }

    pub fn close(&mut self) {
        *self = Self::Closed;
    }
}

impl ToolFailure {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        let message = message.into();
        let classified = ClassifiedToolError {
            code: "invalid_input".to_string(),
            disposition: baml_rt_core::semantics::ErrorDisposition::LlmCorrectable,
            message: message.clone(),
            hint: None,
            retry_after_ms: None,
        };
        let retryability = classified.host_retryability();
        Self {
            kind: ToolFailureKind::InvalidInput,
            message,
            retryability,
            classified,
        }
    }

    pub fn execution_failed(message: impl Into<String>) -> Self {
        let message = message.into();
        let classified = ClassifiedToolError {
            code: "execution_failed".to_string(),
            disposition: baml_rt_core::semantics::ErrorDisposition::Fatal,
            message: message.clone(),
            hint: None,
            retry_after_ms: None,
        };
        let retryability = classified.host_retryability();
        Self {
            kind: ToolFailureKind::ExecutionFailed,
            message,
            retryability,
            classified,
        }
    }

    pub fn from_error(error: &BamlRtError) -> Self {
        Self::from_classified(ClassifiedToolError::from_baml_error(error), error)
    }

    /// Classify using optional per-tool logic from session context.
    pub fn from_error_in_session(
        classifier: &Option<ToolExecutionClassifier>,
        error: &BamlRtError,
    ) -> Self {
        let classified = crate::tool_error_classify::classify_for_session(classifier, error);
        Self::from_classified(classified, error)
    }

    fn from_classified(classified: ClassifiedToolError, error: &BamlRtError) -> Self {
        let kind = tool_failure_kind_from(error);
        let message = classified.message.clone();
        let retryability = classified.host_retryability();
        Self {
            kind,
            message,
            retryability,
            classified,
        }
    }
}

fn tool_failure_kind_from(error: &BamlRtError) -> ToolFailureKind {
    match error {
        BamlRtError::InvalidArgument(_) | BamlRtError::InvalidArgumentWithSource { .. } => {
            ToolFailureKind::InvalidInput
        }
        BamlRtError::QuickJs(_) | BamlRtError::QuickJsWithSource { .. } => {
            ToolFailureKind::ExecutionFailed
        }
        BamlRtError::ToolExecution(_) => ToolFailureKind::ExecutionFailed,
        _ => ToolFailureKind::Unknown,
    }
}

#[derive(Debug)]
pub enum ToolSessionError {
    Transport(BamlRtError),
    Tool(ToolFailure),
}

impl From<BamlRtError> for ToolSessionError {
    fn from(error: BamlRtError) -> Self {
        ToolSessionError::Transport(error)
    }
}

#[derive(Debug, Clone)]
pub enum ToolStep {
    /// More output may follow; session remains open.
    Streaming { output: Value },
    /// Session is yielding output but suspending (e.g. input required). Session remains open; do not call finish.
    Suspended { output: Value },
    /// Session completed; caller may finish or abort.
    Done { output: Option<Value> },
    /// Session failed; caller should abort.
    Error { error: ToolFailure },
}

#[async_trait]
pub trait ToolSession: Send + Sync {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError>;
    async fn read(&mut self, input: Value) -> std::result::Result<ToolStep, ToolSessionError>;
    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError>;
    async fn abort(&mut self, reason: Option<String>) -> std::result::Result<(), ToolSessionError>;
}
