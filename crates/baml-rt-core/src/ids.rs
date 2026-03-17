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
define_id_type!(
    /// Execution session identifier (host-generated, opaque).
    ExecutionSessionId
);

impl ExecutionSessionId {
    /// Create a new execution session ID (host-generated, opaque).
    pub fn new(s: String) -> Self {
        Self(s)
    }
}
define_id_type!(
    /// Intent identifier for planning lineage.
    IntentId
);
define_id_type!(
    /// Plan identifier for planning lineage.
    PlanId
);
define_id_type!(
    /// Plan step identifier within a committed plan.
    PlanStepId
);

/// Deterministic parts used to mint temporal-wire-format ids from digest input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DigestIdParts {
    upper: u64,
    lower: u64,
}

impl DigestIdParts {
    pub const fn new(upper: u64, lower: u64) -> Self {
        Self { upper, lower }
    }

    pub const fn upper(self) -> u64 {
        self.upper
    }

    pub const fn lower(self) -> u64 {
        self.lower
    }
}

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

    /// Uses the `ctx-<u64>-<u64>` wire format with deterministic digest parts
    /// rather than wall-clock millis/counters. This intentionally shares the
    /// normal context-id wire format; callers that care about provenance origin
    /// should rely on surrounding metadata rather than the raw id string.
    pub fn from_digest_parts(parts: DigestIdParts) -> Self {
        Self(TemporalId::new("ctx", parts.upper(), parts.lower()).into_string())
    }

    /// Parses the `ctx-<u64>-<u64>` wire format regardless of whether the
    /// numeric parts originated from wall-clock values or digest-derived ids.
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

    /// Uses the `corr-<u64>-<u64>` wire format with deterministic digest parts
    /// rather than wall-clock millis/counters. This intentionally shares the
    /// normal correlation-id wire format; callers that care about provenance
    /// origin should rely on surrounding metadata rather than the raw id
    /// string.
    pub fn from_digest_parts(parts: DigestIdParts) -> Self {
        Self(TemporalId::new("corr", parts.upper(), parts.lower()).into_string())
    }

    /// Parses the `corr-<u64>-<u64>` wire format regardless of whether the
    /// numeric parts originated from wall-clock values or digest-derived ids.
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

impl From<String> for IntentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for IntentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for PlanId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PlanId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for PlanStepId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PlanStepId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl ExternalConstructible for MessageId {}
impl DerivedConstructible for MessageId {}
impl ExternalConstructible for TaskId {}
impl From<&str> for ContextId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl TemporalConstructible for ContextId {}
impl TemporalConstructible for CorrelationId {}
impl ExternalConstructible for ArtifactId {}
impl MonotonicConstructible for EventId {}
impl UuidConstructible for AgentId {}

#[cfg(test)]
mod tests {
    //! Property tests for the planning/session typed ID newtypes.
    //!
    //! Invariants verified for `IntentId`, `PlanId`, `PlanStepId`, and `ExecutionSessionId`:
    //! - `as_str()` returns the same string used for construction.
    //! - `Display` matches `as_str()`.
    //! - `From<String>` + serde round-trip is lossless.
    //! - `PartialEq` is symmetric (two IDs from the same string are equal).

    use proptest::prelude::*;

    use super::*;

    fn id_strategy() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9\\-]{0,20}".prop_map(|s| s)
    }

    #[test]
    fn digest_id_parts_share_temporal_wire_format() {
        let parts = DigestIdParts::new(42, 7);

        let context = ContextId::from_digest_parts(parts);
        let correlation = CorrelationId::from_digest_parts(parts);

        assert_eq!(context.as_str(), "ctx-42-7");
        assert_eq!(correlation.as_str(), "corr-42-7");
        assert_eq!(ContextId::parse_temporal(context.as_str()), Some(context));
        assert_eq!(
            CorrelationId::parse_temporal(correlation.as_str()),
            Some(correlation)
        );
    }

    proptest! {
        #![proptest_config({
            let mut cfg = ProptestConfig::with_cases(32);
            cfg.failure_persistence = None;
            cfg
        })]

        #[test]
        fn prop_intent_id_round_trip(s in id_strategy()) {
            let id = IntentId::from(s.clone());
            assert_eq!(id.as_str(), s, "as_str must match construction value");
            assert_eq!(id.to_string(), s, "Display must match construction value");
            let json = serde_json::to_value(&id).expect("serialize IntentId");
            let recovered: IntentId = serde_json::from_value(json).expect("deserialize IntentId");
            assert_eq!(recovered, id, "serde round-trip must preserve identity");
            assert_eq!(IntentId::from(s.clone()), id, "PartialEq must be symmetric");
        }

        #[test]
        fn prop_plan_id_round_trip(s in id_strategy()) {
            let id = PlanId::from(s.clone());
            assert_eq!(id.as_str(), s);
            assert_eq!(id.to_string(), s);
            let json = serde_json::to_value(&id).expect("serialize PlanId");
            let recovered: PlanId = serde_json::from_value(json).expect("deserialize PlanId");
            assert_eq!(recovered, id);
        }

        #[test]
        fn prop_plan_step_id_round_trip(s in id_strategy()) {
            let id = PlanStepId::from(s.clone());
            assert_eq!(id.as_str(), s);
            assert_eq!(id.to_string(), s);
            let json = serde_json::to_value(&id).expect("serialize PlanStepId");
            let recovered: PlanStepId = serde_json::from_value(json).expect("deserialize PlanStepId");
            assert_eq!(recovered, id);
        }

        #[test]
        fn prop_execution_session_id_round_trip(s in id_strategy()) {
            let id = ExecutionSessionId::new(s.clone());
            assert_eq!(id.as_str(), s);
            assert_eq!(id.to_string(), s);
            let json = serde_json::to_value(&id).expect("serialize ExecutionSessionId");
            let recovered: ExecutionSessionId = serde_json::from_value(json).expect("deserialize ExecutionSessionId");
            assert_eq!(recovered, id);
        }

        #[test]
        fn prop_distinct_ids_are_not_equal(a in id_strategy(), b in id_strategy()) {
            if a == b { return Ok(()); }
            assert_ne!(IntentId::from(a.clone()), IntentId::from(b.clone()));
            assert_ne!(PlanId::from(a.clone()),   PlanId::from(b.clone()));
            assert_ne!(PlanStepId::from(a.clone()), PlanStepId::from(b.clone()));
        }
    }
}
