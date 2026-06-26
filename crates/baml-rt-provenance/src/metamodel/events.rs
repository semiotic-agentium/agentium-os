// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Per-`EventGraphKind` ZST markers + the `GraphEvent` trait that ties each
//! marker to its primary node label and required-properties bundle.
//!
//! The ZSTs are the spine of the metamodel's typed enforcement: edge-witness
//! impls in [`crate::metamodel::edges`] are parameterised by event marker,
//! and the [`crate::metamodel::writer::MetamodelWriter`] facade is
//! parameterised by event marker, so a Message-arm writer cannot emit an
//! LlmCall-only edge.

use baml_rt_core::ids::ActivityAnchorId;

use crate::{
    graph_model::{EventGraphKind, GraphNodeLabel},
    metamodel::{labels, sealed::Sealed},
};

/// Sealed witness that a marker corresponds to a metamodel event.
///
/// `RuntimeKind` mirrors the marker into the legacy
/// [`EventGraphKind`] enum so that runtime reflection (mappings, debug
/// logging, snapshot tests) stays available alongside the new typed surface.
///
/// `PrimaryLabel` ties the marker to the [`crate::metamodel::labels::NodeLabelTy`]
/// for the event's primary persisted node, matching `EventGraphMapping::primary_node`.
///
/// `RequiredProps` is a nominal struct (no `Option` bag) whose fields are the
/// metamodel's required properties. Constructing the struct without a
/// required field is a compile error.
pub trait GraphEvent: Sealed + Default + 'static {
    const RUNTIME_KIND: EventGraphKind;
    type PrimaryLabel: labels::NodeLabelTy;
    type RequiredProps;

    /// Convenience: the on-disk node label for this event's primary node.
    fn primary_label() -> GraphNodeLabel {
        <Self::PrimaryLabel as labels::NodeLabelTy>::LABEL
    }
}

// ---------------------------------------------------------------------------
// Marker types — one ZST per `EventGraphKind` variant. Macro keeps the
// per-event boilerplate to a single line.
// ---------------------------------------------------------------------------

macro_rules! graph_event {
    ($name:ident => $kind:ident, primary: $label:ident, required: $required:ty) => {
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $name;
        impl Sealed for $name {}
        impl GraphEvent for $name {
            const RUNTIME_KIND: EventGraphKind = EventGraphKind::$kind;
            type PrimaryLabel = labels::$label;
            type RequiredProps = $required;
        }
    };
}

// ---------------------------------------------------------------------------
// Required-properties structs.
//
// Today only the Message arms are fully typed. Other arms accept the
// empty `LegacyRequiredProps` placeholder; migrating them to a nominal
// required-props struct mirroring `MAPPING_*::required_properties` is
// tracked separately and does not affect the legacy free-function write
// path.
// ---------------------------------------------------------------------------

/// Placeholder for event arms that have not yet been migrated to a nominal
/// required-properties struct. Empty by design so it does not constrain
/// the legacy free-function write path.
#[derive(Debug, Default, Clone)]
pub struct LegacyRequiredProps;

/// Required properties for `MessageReceived` events.
///
/// `agent_id` is deliberately absent: agent ownership of a Message is
/// modelled as an EDGE traversal
/// (`Message → MessageProcessing → AgentRuntimeInstance` via
/// `WAS_RECEIVED_BY` + `WAS_EXECUTED_BY`), not as a denormalised
/// `a2a:agent_id` property on the Message row. With this struct as the
/// only typed payload accepted by
/// `MetamodelWriter::<MessageReceived>::commit_primary`, the
/// property-as-relationship shortcut is unrepresentable in the typed
/// write surface.
#[derive(Debug, Clone)]
pub struct MessageReceivedProps {
    pub message_id: crate::metamodel::node_ids::MessageNodeId,
    pub role: String,
    pub content: Vec<String>,
    pub direction: MessageDirection,
}

/// Required properties for `MessageSent` events. Symmetric to
/// [`MessageReceivedProps`].
#[derive(Debug, Clone)]
pub struct MessageSentProps {
    pub message_id: crate::metamodel::node_ids::MessageNodeId,
    pub role: String,
    pub content: Vec<String>,
    pub direction: MessageDirection,
}

/// Closed enumeration of the `a2a:direction` property values. Avoids
/// stringly-typed direction passing across the typed write surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDirection {
    Inbound,
    Outbound,
}

impl MessageDirection {
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Inbound => crate::vocabulary::message_directions::RECEIVED,
            Self::Outbound => crate::vocabulary::message_directions::SENT,
        }
    }
}

// ---------------------------------------------------------------------------
// Task status — typed enum + typed required-props bundle for
// TaskStatusChanged events.
// ---------------------------------------------------------------------------

/// Newtype guaranteeing a non-empty `String`. Used by [`TaskStatusKind::Failed`]
/// so `(Failed, reason = "")` is structurally unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NonEmptyString(String);

/// Construction error for [`NonEmptyString`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("string must be non-empty")]
pub struct EmptyStringError;

impl NonEmptyString {
    /// Construct from any `Into<String>`. Returns [`EmptyStringError`] if
    /// the input is empty after trimming. Trim semantics chosen because
    /// the wire formats this guards (LLM error reasons, status payloads)
    /// can carry whitespace-only "errors" that callers should treat as
    /// absent rather than as a real reason.
    pub fn new(raw: impl Into<String>) -> Result<Self, EmptyStringError> {
        let s = raw.into();
        if s.trim().is_empty() {
            Err(EmptyStringError)
        } else {
            Ok(Self(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for NonEmptyString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Closed enumeration of A2A task statuses. Each variant carries the
/// payload that is *only* meaningful for that status, so combinations
/// like `(InputRequired, prompt = None)` or `(Failed, reason = "")` are
/// structurally unrepresentable.
///
/// `metadata` and `extra` wire-blob fields are deliberately absent
/// (Fabricator decree, "narrowly-typed fields only"). `#[non_exhaustive]`
/// plus the absence of any `serde_json::Value`-typed field locks future
/// PRs out of relaxing this without a doctrinal review.
///
/// The mapping table from the wire `TaskStatus` JSON to `TaskStatusKind`
/// lives in [`crate::normalizer`]'s `TaskStatusChanged` arm.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskStatusKind {
    Submitted,
    Working,
    InputRequired { prompt: String },
    AuthRequired,
    Completed,
    Failed { reason: NonEmptyString },
    Canceled,
    Rejected,
}

impl TaskStatusKind {
    /// Stable wire string for the variant tag (independent of the
    /// per-variant payload). Matches the legacy `a2a:task_state` enum on
    /// disk so existing read code can transition incrementally.
    pub const fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Submitted => "TASK_STATE_SUBMITTED",
            Self::Working => "TASK_STATE_WORKING",
            Self::InputRequired { .. } => "TASK_STATE_INPUT_REQUIRED",
            Self::AuthRequired => "TASK_STATE_AUTH_REQUIRED",
            Self::Completed => "TASK_STATE_COMPLETED",
            Self::Failed { .. } => "TASK_STATE_FAILED",
            Self::Canceled => "TASK_STATE_CANCELED",
            Self::Rejected => "TASK_STATE_REJECTED",
        }
    }

    /// Whether the variant represents a terminal status (no further
    /// transitions are expected). Drives the broadcaster's `retire_task`
    /// signal in Phase B.
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Canceled | Self::Rejected
        )
    }
}

impl std::fmt::Display for TaskStatusKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// Required properties for `TaskStatusChanged` events. Replaces the
/// untyped `LegacyRequiredProps` placeholder so the `(status, payload)`
/// invariant is enforced at construction (via [`TaskStatusKind`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct A2ATaskStateProps {
    /// On-disk `node_id` of the owning Task entity. Used by the
    /// normalizer's head-pointer re-point logic (Phase A5) to identify
    /// the `from_id` of the `WAS_LAST_TRANSITIONED_TO` edge.
    pub task: crate::metamodel::node_ids::TaskNodeId,
    /// The new status this transition writes.
    pub new_status: TaskStatusKind,
    /// The previous status, when known. `None` only for the first
    /// transition emitted for a Task.
    pub old_status: Option<TaskStatusKind>,
    /// Wall-clock timestamp of the transition (milliseconds since epoch).
    pub transitioned_at_ms: u64,
    /// Originating activity anchor for this immutable TaskState node.
    pub activity_anchor: ActivityAnchorId,
}

impl A2ATaskStateProps {
    /// Construct an `A2ATaskStateProps` with all required fields. The
    /// struct is `#[non_exhaustive]` so callers outside this crate must
    /// route through this constructor; future field additions surface
    /// as a constructor signature change rather than as silent default
    /// values.
    pub fn new(
        task: crate::metamodel::node_ids::TaskNodeId,
        new_status: TaskStatusKind,
        old_status: Option<TaskStatusKind>,
        transitioned_at_ms: u64,
        activity_anchor: ActivityAnchorId,
    ) -> Self {
        Self {
            task,
            new_status,
            old_status,
            transitioned_at_ms,
            activity_anchor,
        }
    }
}

// ---------------------------------------------------------------------------
// Event marker declarations
// ---------------------------------------------------------------------------

graph_event!(IntentResolved => IntentResolved, primary: Intent, required: LegacyRequiredProps);
graph_event!(PlanGenerated => PlanGenerated, primary: Plan, required: LegacyRequiredProps);
graph_event!(PlanStepStatusChanged => PlanStepStatusChanged, primary: PlanStep, required: LegacyRequiredProps);
graph_event!(LlmCallStarted => LlmCallStarted, primary: LlmCall, required: LegacyRequiredProps);
graph_event!(LlmCallCompleted => LlmCallCompleted, primary: LlmCall, required: LegacyRequiredProps);
graph_event!(PromptRejected => PromptRejected, primary: PromptRejected, required: LegacyRequiredProps);
graph_event!(ToolCallStarted => ToolCallStarted, primary: ToolCall, required: LegacyRequiredProps);
graph_event!(ToolCallCompleted => ToolCallCompleted, primary: ToolCall, required: LegacyRequiredProps);
graph_event!(AgentBooted => AgentBooted, primary: AgentBoot, required: LegacyRequiredProps);
graph_event!(AgentStopped => AgentStopped, primary: AgentStop, required: LegacyRequiredProps);
graph_event!(TaskExists => TaskExists, primary: Task, required: LegacyRequiredProps);
graph_event!(TaskExecutionStarted => TaskExecutionStarted, primary: TaskExecution, required: LegacyRequiredProps);
graph_event!(TaskExecutionEnded => TaskExecutionEnded, primary: TaskExecution, required: LegacyRequiredProps);
graph_event!(TaskStatusChanged => TaskStatusChanged, primary: TaskState, required: A2ATaskStateProps);
graph_event!(TaskArtifactGenerated => TaskArtifactGenerated, primary: Artifact, required: LegacyRequiredProps);
graph_event!(MessageReceived => MessageReceived, primary: Message, required: MessageReceivedProps);
graph_event!(MessageSent => MessageSent, primary: Message, required: MessageSentProps);
graph_event!(ToolSessionStep => ToolSessionStep, primary: SessionStep, required: LegacyRequiredProps);
graph_event!(ExternalToolLifecycle => ExternalToolLifecycle, primary: ToolCall, required: LegacyRequiredProps);
graph_event!(CallbackDispatchContextsLinked => CallbackDispatchContextsLinked, primary: Task, required: LegacyRequiredProps);
graph_event!(HostSourcePollRecorded => HostSourcePollRecorded, primary: Message, required: LegacyRequiredProps);
graph_event!(HostDispatchAccepted => HostDispatchAccepted, primary: Message, required: LegacyRequiredProps);
graph_event!(HostDispatchRejected => HostDispatchRejected, primary: Message, required: LegacyRequiredProps);
graph_event!(ContextCompactionRecorded => ContextCompactionRecorded, primary: ContextCompaction, required: LegacyRequiredProps);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_model::ALL_EVENT_KINDS;

    #[test]
    fn every_event_kind_has_a_typed_marker() {
        // Visit every EventGraphKind through the typed surface to ensure no
        // variant is missing a marker.
        let _ = (
            IntentResolved::RUNTIME_KIND,
            PlanGenerated::RUNTIME_KIND,
            PlanStepStatusChanged::RUNTIME_KIND,
            LlmCallStarted::RUNTIME_KIND,
            LlmCallCompleted::RUNTIME_KIND,
            PromptRejected::RUNTIME_KIND,
            ToolCallStarted::RUNTIME_KIND,
            ToolCallCompleted::RUNTIME_KIND,
            AgentBooted::RUNTIME_KIND,
            AgentStopped::RUNTIME_KIND,
            TaskExists::RUNTIME_KIND,
            TaskExecutionStarted::RUNTIME_KIND,
            TaskExecutionEnded::RUNTIME_KIND,
            TaskStatusChanged::RUNTIME_KIND,
            TaskArtifactGenerated::RUNTIME_KIND,
            MessageReceived::RUNTIME_KIND,
            MessageSent::RUNTIME_KIND,
            ToolSessionStep::RUNTIME_KIND,
            ExternalToolLifecycle::RUNTIME_KIND,
            CallbackDispatchContextsLinked::RUNTIME_KIND,
            HostSourcePollRecorded::RUNTIME_KIND,
            HostDispatchAccepted::RUNTIME_KIND,
            HostDispatchRejected::RUNTIME_KIND,
            ContextCompactionRecorded::RUNTIME_KIND,
        );

        // Reflective check: typed markers must equal the runtime list of EventGraphKind variants.
        assert_eq!(ALL_EVENT_KINDS.len(), 24);
    }

    #[test]
    fn message_props_carry_required_fields_and_no_agent_id() {
        let props = MessageReceivedProps {
            message_id: crate::metamodel::node_ids::MessageNodeId::new("msg-1"),
            role: "ROLE_USER".into(),
            content: vec!["hello".into()],
            direction: MessageDirection::Inbound,
        };
        assert_eq!(props.message_id.as_str(), "msg-1");
        assert_eq!(MessageDirection::Inbound.as_wire_str(), "received");
        // The struct deliberately has no `agent_id` field — agent
        // ownership is an EDGE traversal, not a Message property, and
        // is unrepresentable in this typed payload.
    }

    #[test]
    fn message_received_primary_label_is_message() {
        assert_eq!(MessageReceived::primary_label(), GraphNodeLabel::Message);
    }

    #[test]
    fn non_empty_string_rejects_empty_and_whitespace() {
        assert!(NonEmptyString::new("").is_err());
        assert!(NonEmptyString::new("   ").is_err());
        assert!(NonEmptyString::new("\t\n").is_err());
        let s = NonEmptyString::new("oops").expect("non-empty");
        assert_eq!(s.as_str(), "oops");
    }

    #[test]
    fn task_status_kind_terminal_set_matches_doctrine() {
        assert!(TaskStatusKind::Completed.is_terminal());
        assert!(TaskStatusKind::Canceled.is_terminal());
        assert!(TaskStatusKind::Rejected.is_terminal());
        assert!(
            TaskStatusKind::Failed {
                reason: NonEmptyString::new("boom").unwrap()
            }
            .is_terminal()
        );
        assert!(!TaskStatusKind::Submitted.is_terminal());
        assert!(!TaskStatusKind::Working.is_terminal());
        assert!(
            !TaskStatusKind::InputRequired {
                prompt: "tell me more".into()
            }
            .is_terminal()
        );
        assert!(!TaskStatusKind::AuthRequired.is_terminal());
    }

    #[test]
    fn task_status_kind_input_required_payload_is_required_at_construction() {
        // The struct literal MUST supply `prompt`; this test fails to
        // compile if the variant were ever relaxed to `prompt: Option<String>`.
        let kind = TaskStatusKind::InputRequired {
            prompt: "what is your name?".into(),
        };
        match kind {
            TaskStatusKind::InputRequired { prompt } => {
                assert_eq!(prompt, "what is your name?");
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn task_status_kind_failed_carries_non_empty_reason() {
        let kind = TaskStatusKind::Failed {
            reason: NonEmptyString::new("network timeout").expect("non-empty"),
        };
        if let TaskStatusKind::Failed { reason } = kind {
            assert_eq!(reason.as_str(), "network timeout");
        } else {
            panic!("variant mismatch");
        }
    }

    #[test]
    fn task_state_props_uses_typed_status_kind() {
        let props = A2ATaskStateProps {
            task: crate::metamodel::node_ids::TaskNodeId::new("task:t1"),
            new_status: TaskStatusKind::Working,
            old_status: Some(TaskStatusKind::Submitted),
            transitioned_at_ms: 1_700_000_000_000,
            activity_anchor: baml_rt_core::ids::ActivityAnchorId::from_counter(9),
        };
        assert_eq!(props.task.as_str(), "task:t1");
        assert_eq!(props.new_status.as_wire_str(), "TASK_STATE_WORKING");
        assert_eq!(
            props.old_status.as_ref().map(TaskStatusKind::as_wire_str),
            Some("TASK_STATE_SUBMITTED")
        );
    }
}
