// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Translate [`TaskUpdateFrame`]s emitted by
//! [`crate::task_update_broadcaster::TaskUpdateBroadcaster`] into the
//! wire-shaped [`TaskUpdateEvent`]s the SSE handler emits, and drain a
//! [`crate::task_update_session::TaskUpdateSession`] into the same
//! `Vec<TaskUpdateEvent>` shape used at the final wire boundary.
//!
//! This module translates replay/live task updates into the wire event
//! shapes used by `tasks/subscribe`. The durable source is the
//! provenance-backed [`TaskUpdateSession`]; callers can drain replay
//! into `TaskUpdateEvent` values or translate frames inline.

use std::collections::HashMap;

use baml_rt_core::ids::{ContextId, MessageId};
use baml_rt_provenance::ReplayError;

use crate::{
    a2a_store::TaskUpdateEvent,
    a2a_types::{
        A2aMessageId, Artifact, Message, MessageRole, Part, TaskArtifactUpdateEvent, TaskState,
        TaskStatus, TaskStatusUpdateEvent,
    },
    task_update_broadcaster::{ArtifactRef, MessageRef, TaskUpdateFrame},
    task_update_session::TaskUpdateSession,
};

/// Map a single [`TaskUpdateFrame`] to the wire `TaskUpdateEvent` shape
/// the SSE serialiser emits. Returns `None` for frame variants that do
/// not have a wire representation in the existing
/// [`TaskUpdateEvent`] enum (currently: `MessageReceived` /
/// `MessageSent`, which the SSE handler delivers as `message` chunks
/// through a separate code path).
pub fn frame_to_task_update_event(frame: TaskUpdateFrame) -> Option<TaskUpdateEvent> {
    match frame {
        TaskUpdateFrame::StatusTransition { state, .. } => {
            Some(TaskUpdateEvent::Status(status_props_to_event(state)))
        }
        TaskUpdateFrame::ArtifactGenerated { artifact, .. } => {
            Some(TaskUpdateEvent::Artifact(artifact_ref_to_event(artifact)))
        }
        TaskUpdateFrame::MessageReceived { .. } | TaskUpdateFrame::MessageSent { .. } => None,
        // `TaskUpdateFrame` is `#[non_exhaustive]`; a future variant
        // (e.g. SessionStep delivery) must be wired through the SSE
        // serialiser explicitly. Until then, keep it off the wire.
        _ => None,
    }
}

/// Drain the replay leg of `session` into `Vec<TaskUpdateEvent>`. Frame
/// variants without a wire `TaskUpdateEvent` representation are dropped
/// silently (see [`frame_to_task_update_event`]); callers that need
/// access to the raw frames should call
/// [`TaskUpdateSession::drain_replay`] directly and translate inline.
pub async fn drain_replay_into_events(
    session: &mut TaskUpdateSession,
) -> Result<Vec<TaskUpdateEvent>, ReplayError> {
    let frames = session.drain_replay().await?;
    Ok(frames
        .into_iter()
        .filter_map(frame_to_task_update_event)
        .collect())
}

fn status_props_to_event(
    state: baml_rt_provenance::metamodel::A2ATaskStateProps,
) -> TaskStatusUpdateEvent {
    let baml_rt_provenance::metamodel::A2ATaskStateProps {
        task,
        new_status,
        transitioned_at_ms,
        ..
    } = state;

    let prompt_message = match &new_status {
        baml_rt_provenance::metamodel::TaskStatusKind::InputRequired { prompt } => {
            Some(prompt_to_message(prompt))
        }
        _ => None,
    };

    let mut extra: HashMap<String, serde_json::Value> = HashMap::new();
    if let baml_rt_provenance::metamodel::TaskStatusKind::Failed { reason } = &new_status {
        extra.insert(
            "error_reason".to_string(),
            serde_json::Value::String(reason.as_str().to_string()),
        );
    }

    let status = TaskStatus {
        state: Some(TaskState::String(new_status.as_wire_str().to_string())),
        message: prompt_message,
        timestamp: Some(transitioned_at_ms.to_string()),
        extra: HashMap::new(),
    };

    TaskStatusUpdateEvent {
        context_id: None,
        task_id: Some(task.to_task_id()),
        status: Some(status),
        metadata: None,
        extra,
    }
}

fn artifact_ref_to_event(artifact: ArtifactRef) -> TaskArtifactUpdateEvent {
    let ArtifactRef {
        task_id,
        artifact_id,
        artifact_type,
    } = artifact;
    TaskArtifactUpdateEvent {
        context_id: None,
        task_id: Some(task_id),
        last_chunk: None,
        append: None,
        artifact: Some(Artifact {
            artifact_id,
            name: None,
            description: None,
            parts: Vec::new(),
            extensions: Vec::new(),
            metadata: artifact_type.map(|t| {
                let mut map = HashMap::new();
                map.insert("artifact_type".to_string(), serde_json::Value::String(t));
                map
            }),
            extra: HashMap::new(),
        }),
        metadata: None,
        extra: HashMap::new(),
    }
}

fn prompt_to_message(prompt: &str) -> Message {
    Message {
        message_id: A2aMessageId::incoming(baml_rt_core::ids::ExternalId::new(format!(
            "input-required:{}",
            uuid::Uuid::new_v4()
        ))),
        role: MessageRole::Agent,
        parts: vec![Part {
            text: Some(prompt.to_string()),
            ..Default::default()
        }],
        context_id: None,
        task_id: None,
        reference_task_ids: Vec::new(),
        extensions: Vec::new(),
        metadata: None,
        extra: HashMap::new(),
    }
}

// Suppress unused-import warnings for translator helpers that are only
// referenced from `prompt_to_message` and the test module — keeping the
// imports asserts at compile-time that the wire types still exist.
const _MESSAGE_REF_USES: Option<&MessageRef> = None;
const _CONTEXT_ID_USES: Option<&ContextId> = None;
const _MESSAGE_ID_USES: Option<&MessageId> = None;

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{ArtifactId, ExternalId, TaskId};
    use baml_rt_provenance::metamodel::{
        A2ATaskStateProps, NonEmptyString, TaskNodeId, TaskStatusKind,
    };

    use super::*;
    use crate::task_update_broadcaster::ArtifactRef;

    fn anchor() -> baml_rt_core::ids::ActivityAnchorId {
        baml_rt_core::ids::ActivityAnchorId::from_counter(7)
    }

    #[test]
    fn status_changed_translates_to_status_event_with_wire_state() {
        let frame = TaskUpdateFrame::StatusTransition {
            state: A2ATaskStateProps::new(
                TaskNodeId::new("task:t-drain"),
                TaskStatusKind::Working,
                Some(TaskStatusKind::Submitted),
                123,
                anchor(),
            ),
            cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor()).expect("cursor"),
        };
        let event = frame_to_task_update_event(frame).expect("status -> event");
        match event {
            TaskUpdateEvent::Status(s) => {
                let state = s.status.expect("status").state.expect("state");
                match state {
                    TaskState::String(v) => assert_eq!(v, "TASK_STATE_WORKING"),
                    _ => panic!("expected string state"),
                }
                let task_id = s.task_id.expect("task_id");
                assert_eq!(task_id.as_str(), "t-drain", "task: prefix is stripped");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn input_required_carries_prompt_message() {
        let frame = TaskUpdateFrame::StatusTransition {
            state: A2ATaskStateProps::new(
                TaskNodeId::new("task:t-input"),
                TaskStatusKind::InputRequired {
                    prompt: "what is your name?".into(),
                },
                Some(TaskStatusKind::Working),
                500,
                anchor(),
            ),
            cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor()).expect("cursor"),
        };
        let event = frame_to_task_update_event(frame).expect("status -> event");
        let TaskUpdateEvent::Status(s) = event else {
            panic!("expected Status variant");
        };
        let msg = s
            .status
            .and_then(|st| st.message)
            .expect("status carries prompt message");
        // Inspect the first part for the prompt text.
        let part = msg.parts.first().expect("at least one part");
        let serialised = serde_json::to_value(part).expect("serialise");
        assert!(
            serialised.to_string().contains("what is your name?"),
            "prompt body must round-trip into the message: {serialised}"
        );
    }

    #[test]
    fn failed_writes_error_reason_into_extra() {
        let frame = TaskUpdateFrame::StatusTransition {
            state: A2ATaskStateProps::new(
                TaskNodeId::new("task:t-fail"),
                TaskStatusKind::Failed {
                    reason: NonEmptyString::new("downstream timeout").expect("non-empty"),
                },
                Some(TaskStatusKind::Working),
                900,
                anchor(),
            ),
            cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor()).expect("cursor"),
        };
        let event = frame_to_task_update_event(frame).expect("status -> event");
        let TaskUpdateEvent::Status(s) = event else {
            panic!("expected Status variant");
        };
        let reason = s
            .extra
            .get("error_reason")
            .and_then(serde_json::Value::as_str)
            .expect("failed status carries reason");
        assert_eq!(reason, "downstream timeout");
    }

    #[test]
    fn artifact_translates_to_artifact_event() {
        let frame = TaskUpdateFrame::ArtifactGenerated {
            artifact: ArtifactRef {
                task_id: TaskId::from_external(ExternalId::new("t-art")),
                artifact_id: Some(ArtifactId::from_external(ExternalId::new("a-99"))),
                artifact_type: Some("plan".to_string()),
            },
            cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor()).expect("cursor"),
        };
        let event = frame_to_task_update_event(frame).expect("artifact -> event");
        let TaskUpdateEvent::Artifact(a) = event else {
            panic!("expected Artifact variant");
        };
        let artifact = a.artifact.expect("artifact");
        assert_eq!(
            artifact.artifact_id.as_ref().map(ArtifactId::as_str),
            Some("a-99")
        );
        let metadata = artifact.metadata.expect("type metadata");
        assert_eq!(
            metadata
                .get("artifact_type")
                .and_then(serde_json::Value::as_str),
            Some("plan")
        );
    }

    #[test]
    fn message_variants_have_no_wire_representation() {
        let frame = TaskUpdateFrame::MessageReceived {
            message: MessageRef {
                context_id: ContextId::new(1, 1),
                message_id: MessageId::from_external(ExternalId::new("m-1")),
            },
            cursor: baml_rt_provenance::TaskReplayCursor::from_anchor(anchor()).expect("cursor"),
        };
        assert!(
            frame_to_task_update_event(frame).is_none(),
            "MessageReceived has no TaskUpdateEvent representation"
        );
    }
}
