//! Strongly-typed ID wrappers for domain concepts.
//!
//! These newtypes prevent mixing different ID types at compile time,
//! following the production-rust.md guidelines for strong types at boundaries.

use std::fmt;

pub use baml_rt_id::{
    ConstantConstructible, ConstantId, DerivedConstructible, DerivedId, ExternalConstructible,
    ExternalId, MonotonicConstructible, MonotonicId, ProvActivitySemantics, ProvAgentSemantics,
    ProvConstantActivitySemantics, ProvConstantAgentSemantics, ProvConstantEntitySemantics,
    ProvConstantIdTemplate, ProvDerivedActivitySemantics, ProvDerivedAgentSemantics,
    ProvDerivedEntitySemantics, ProvDerivedIdTemplate, ProvEntitySemantics, ProvIdSemantics,
    ProvKind, ProvVocabularyType, TemporalConstructible, TemporalId, UuidConstructible, UuidId,
};
use serde::{Deserialize, Serialize};

macro_rules! define_id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id_type!(
    /// Message identifier for A2A messages (external protocol id).
    MessageId
);
define_id_type!(
    /// Task identifier for A2A tasks (external protocol id).
    TaskId
);
define_id_type!(
    /// Context identifier for execution contexts.
    ContextId
);
define_id_type!(
    /// Correlation identifier for distributed tracing.
    CorrelationId
);
define_id_type!(
    /// Artifact identifier for task artifacts.
    ArtifactId
);
define_id_type!(
    /// Event identifier for provenance events.
    EventId
);
define_id_type!(
    /// Agent runtime instance identifier.
    AgentId
);

impl MessageId {
    pub fn from_external(id: ExternalId) -> Self {
        Self(id.into_string())
    }

    pub fn from_derived(id: DerivedId) -> Self {
        Self(id.into_string())
    }
}

impl TaskId {
    pub fn from_external(id: ExternalId) -> Self {
        Self(id.into_string())
    }
}

impl ContextId {
    pub fn new(millis: u64, counter: u64) -> Self {
        Self(TemporalId::new("ctx", millis, counter).into_string())
    }

    pub fn parse_temporal(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix("ctx-")?;
        let mut parts = rest.splitn(2, '-');
        let millis = parts.next()?.parse::<u64>().ok()?;
        let counter = parts.next()?.parse::<u64>().ok()?;
        Some(Self::new(millis, counter))
    }
}

impl CorrelationId {
    pub fn new(millis: u64, counter: u64) -> Self {
        Self(TemporalId::new("corr", millis, counter).into_string())
    }

    pub fn parse_temporal(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix("corr-")?;
        let mut parts = rest.splitn(2, '-');
        let millis = parts.next()?.parse::<u64>().ok()?;
        let counter = parts.next()?.parse::<u64>().ok()?;
        Some(Self::new(millis, counter))
    }
}

impl ArtifactId {
    pub fn from_external(id: ExternalId) -> Self {
        Self(id.into_string())
    }
}

impl EventId {
    pub fn from_counter(counter: u64) -> Self {
        Self(MonotonicId::new("prov", counter).into_string())
    }
}

impl From<String> for EventId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for EventId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for MessageId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for MessageId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
impl AgentId {
    pub fn from_uuid(id: UuidId) -> Self {
        Self(id.to_string())
    }
}

impl ExternalConstructible for MessageId {}
impl DerivedConstructible for MessageId {}
impl ExternalConstructible for TaskId {}
impl TemporalConstructible for ContextId {}
impl TemporalConstructible for CorrelationId {}
impl ExternalConstructible for ArtifactId {}
impl MonotonicConstructible for EventId {}
impl UuidConstructible for AgentId {}
