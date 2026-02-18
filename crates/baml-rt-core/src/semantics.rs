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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Success,
    Failure,
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
