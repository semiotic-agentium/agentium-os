// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
    /// Correlation key for a **provenance activity emission** in the append-only stream.
    ///
    /// Same string as Surreal property `a2a_activity_anchor` and colon-key `a2a:activity_anchor`
    /// on stored nodes (message handling, tool invocation, LLM call, …).
    ActivityAnchorId
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

    /// Host-minted live stream task for the first `message.sendStream` turn when the
    /// caller did not supply an explicit task id.
    pub fn for_live_stream(context_id: &ContextId, message_id: &MessageId) -> Self {
        Self::from_external(ExternalId::new(format!(
            "live-task:{}:{}",
            context_id.as_str(),
            message_id.as_str()
        )))
    }

    /// Synthetic task id for CLI/test-only task scopes.
    pub fn for_synthetic_counter(counter: u64) -> Self {
        Self::from_external(ExternalId::new(format!("syn-task-{counter}")))
    }

    /// Fallback task id when JS emits task-bearing chunks without an explicit task identity.
    pub fn for_js_runtime(id: UuidId) -> Self {
        Self::from_external(ExternalId::new(format!("js-task-{id}")))
    }

    /// Child task id for delegated internal A2A sessions.
    pub fn for_delegated_child(id: UuidId) -> Self {
        Self::from_external(ExternalId::new(format!("a2a-child-{id}")))
    }

    /// Stable task id for the runner stdio conversation scope.
    pub fn for_stdio_context(context_id: &ContextId) -> Self {
        Self::from_external(ExternalId::new(format!("cli-task-{}", context_id.as_str())))
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

    /// Stable delegated child context for an internal A2A session.
    ///
    /// The child task id is part of the encoding so parallel delegated sessions
    /// from the same caller to the same target do not collide on one stream key.
    pub fn for_a2a_child(
        caller_context_id: &ContextId,
        target_package: &str,
        target_instance_id: &str,
        child_task_id: &TaskId,
    ) -> Self {
        Self(format!(
            "a2a:{caller}:{pkg}/{inst}:{child}",
            caller = caller_context_id.as_str(),
            pkg = target_package,
            inst = target_instance_id,
            child = child_task_id.as_str(),
        ))
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

impl ActivityAnchorId {
    pub fn from_counter(counter: u64) -> Self {
        Self(MonotonicId::new("prov", counter).into_string())
    }
}

impl From<String> for ActivityAnchorId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ActivityAnchorId {
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
impl MonotonicConstructible for ActivityAnchorId {}
impl UuidConstructible for AgentId {}

macro_rules! impl_id_json_schema_and_ts {
    ($($name:ident),* $(,)?) => {
        $(
        impl baml_derive_core::JsonSchemaType for $name {
            fn json_schema_inline() -> serde_json::Value {
                serde_json::json!({"type": "string"})
            }
        }

        impl baml_derive_core::TsType for $name {
            fn ts_type_name() -> &'static str {
                stringify!($name)
            }

            fn ts_decl() -> Option<String> {
                Some(::std::format!(
                    "export type {} = string;",
                    stringify!($name)
                ))
            }
        }
        )*
    };
}

impl_id_json_schema_and_ts!(
    MessageId,
    TaskId,
    ContextId,
    CorrelationId,
    ArtifactId,
    ActivityAnchorId,
    AgentId,
    ExecutionSessionId,
    IntentId,
    PlanId,
    PlanStepId
);

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

    #[test]
    fn task_id_live_stream_constructor_is_deterministic() {
        let context_id = ContextId::new(730, 2);
        let message_id = MessageId::from("dispatch-echo-resume-msg");
        let task_id = TaskId::for_live_stream(&context_id, &message_id);
        assert_eq!(
            task_id.as_str(),
            "live-task:ctx-730-2:dispatch-echo-resume-msg"
        );
    }

    #[test]
    fn task_id_named_runtime_constructors_encode_expected_prefixes() {
        let context_id = ContextId::new(731, 1);
        let uuid = UuidId::parse_str("00000000-0000-0000-0000-000000000123").expect("uuid");
        assert_eq!(TaskId::for_synthetic_counter(7).as_str(), "syn-task-7");
        assert_eq!(
            TaskId::for_js_runtime(uuid).as_str(),
            "js-task-00000000-0000-0000-0000-000000000123"
        );
        let uuid = UuidId::parse_str("00000000-0000-0000-0000-000000000456").expect("uuid");
        assert_eq!(
            TaskId::for_delegated_child(uuid).as_str(),
            "a2a-child-00000000-0000-0000-0000-000000000456"
        );
        assert_eq!(
            TaskId::for_stdio_context(&context_id).as_str(),
            "cli-task-ctx-731-1"
        );
    }

    #[test]
    fn context_id_a2a_child_constructor_is_stable_per_child_task() {
        let caller = ContextId::new(88, 1);
        let child_task_id = TaskId::from_external(ExternalId::new("a2a-child-fixed".to_string()));
        let context_id =
            ContextId::for_a2a_child(&caller, "responder-agent", "default", &child_task_id);
        assert_eq!(
            context_id.as_str(),
            "a2a:ctx-88-1:responder-agent/default:a2a-child-fixed"
        );
    }
}
