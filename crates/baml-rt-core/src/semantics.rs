use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InvocationKind {
    Invoke,
    Stream,
}

impl InvocationKind {
    pub const fn is_stream(self) -> bool {
        matches!(self, Self::Stream)
    }
}

impl From<bool> for InvocationKind {
    fn from(value: bool) -> Self {
        if value { Self::Stream } else { Self::Invoke }
    }
}

impl From<InvocationKind> for bool {
    fn from(value: InvocationKind) -> Self {
        value.is_stream()
    }
}

/// Outcome of a completed activity (e.g. tool call). Binary: success or failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Success,
    Failure,
}

/// State of an activity (e.g. tool call). Inferred from (1) activity having an end time and (2) outcome.
/// InProgress when no end time; Success | Failed when completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityOutcome {
    InProgress,
    Success,
    Failed,
}

impl ActivityOutcome {
    pub const fn is_completed(self) -> bool {
        matches!(self, Self::Success | Self::Failed)
    }
}

impl Outcome {
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

impl From<bool> for Outcome {
    fn from(value: bool) -> Self {
        if value { Self::Success } else { Self::Failure }
    }
}

impl From<Outcome> for bool {
    fn from(value: Outcome) -> Self {
        value.is_success()
    }
}

impl Serialize for Outcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bool(self.is_success())
    }
}

impl<'de> Deserialize<'de> for Outcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = bool::deserialize(deserializer)?;
        Ok(Outcome::from(value))
    }
}

/// How a failure should be handled by the host runtime vs the LLM.
///
/// Distinct from [`Retryability`]: JSON-RPC / client retry hints are derived from this
/// (e.g. [`ErrorDisposition::HostRetriable`] maps to retryable for transport-level retries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorDisposition {
    /// Transient infrastructure / rate limits — host may retry without a new LLM turn.
    HostRetriable,
    /// Bad args or schema — return structured error to the model; no blind host retry.
    LlmCorrectable,
    /// Definitive failure for this call (auth, not found) — inform the model; session/turn may continue.
    InformAndContinue,
    /// Unrecoverable for the current execution path (abort as today).
    Fatal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retryability {
    Retryable,
    Permanent,
}

impl Retryability {
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Retryable)
    }
}

impl From<bool> for Retryability {
    fn from(value: bool) -> Self {
        if value {
            Self::Retryable
        } else {
            Self::Permanent
        }
    }
}

impl From<Retryability> for bool {
    fn from(value: Retryability) -> Self {
        value.is_retryable()
    }
}

impl Serialize for Retryability {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bool(self.is_retryable())
    }
}

impl<'de> Deserialize<'de> for Retryability {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = bool::deserialize(deserializer)?;
        Ok(Retryability::from(value))
    }
}
