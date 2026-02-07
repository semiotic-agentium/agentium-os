//! Tool session FSM primitives.

use baml_rt_core::BamlRtError;
use async_trait::async_trait;
use serde_json::Value;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use uuid::Uuid;

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
        let uuid = Uuid::parse_str(&value).map_err(|_| {
            BamlRtError::InvalidArgument(format!("Invalid tool session id '{}'", value))
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
    pub retryable: bool,
}

impl ToolFailure {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            kind: ToolFailureKind::InvalidInput,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn execution_failed(message: impl Into<String>) -> Self {
        Self {
            kind: ToolFailureKind::ExecutionFailed,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn from_error(error: &BamlRtError) -> Self {
        let kind = match error {
            BamlRtError::InvalidArgument(_) | BamlRtError::InvalidArgumentWithSource { .. } => {
                ToolFailureKind::InvalidInput
            }
            BamlRtError::QuickJs(_) | BamlRtError::QuickJsWithSource { .. } => {
                ToolFailureKind::ExecutionFailed
            }
            BamlRtError::ToolExecution(_) => ToolFailureKind::ExecutionFailed,
            _ => ToolFailureKind::Unknown,
        };
        Self {
            kind,
            message: error.to_string(),
            retryable: false,
        }
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
    Streaming { output: Value },
    Done { output: Option<Value> },
    Error { error: ToolFailure },
}

#[async_trait]
pub trait ToolSession: Send + Sync {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError>;
    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError>;
    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError>;
    async fn abort(&mut self, reason: Option<String>) -> std::result::Result<(), ToolSessionError>;
}
