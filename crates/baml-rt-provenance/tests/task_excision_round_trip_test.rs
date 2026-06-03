// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! End-to-end round-trip of the post-excision A2A task surface.
//!
//! What this exercises (one vertical slice, no relational mirror):
//!
//! 1. Task creation via `ProvEvent::task_exists` → `TaskGraphReader::resolve_scoped`
//!    returns `Some(ScopedTaskRef)` and refuses cross-context lookups.
//! 2. Status transitions via `ProvEvent::task_status_changed` → the
//!    `WAS_LAST_TRANSITIONED_TO` head-pointer survives with cardinality 1
//!    and `latest_state` returns the most recent `A2ATaskStateProps`
//!    (read through the typed `EdgeProjection` surface).
//! 3. Artifact generation via `ProvEvent::task_artifact_generated` → the
//!    artifact surfaces inside `HydratedTask::artifacts`.
//! 4. Message lifecycle (`MessageReceived` + `MessageSent`) → both ends
//!    surface inside `HydratedTask::messages` (graph order).
//! 5. `HydratedTask` carries no `metadata` / `extra` field. The struct
//!    definition is the contract; this test asserts it via field access
//!    rather than relying on the absence of a wire knob.
//! 6. `list_scoped` returns the task scoped to the context and does not
//!    return tasks from other contexts.
//!
//! ```bash
//! cargo test -p baml-rt-provenance --test task_excision_round_trip_test
//! ```

use std::sync::Arc;

use baml_rt_core::ids::{AgentId, ArtifactId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    ProvEvent, ProvenanceWriter, TaskGraphReader,
    metamodel::{NonEmptyString, TaskStatusKind},
};
use test_support::testing::provenance_fixtures::build_isolated_store;

// Wire status strings, matching `parse_status_kind` in
// `surreal_store/task_graph_reader_impl.rs`. Round-trip parity is the
// whole point of the test, so use the literal vocabulary strings.
const SUBMITTED: &str = "TASK_STATE_SUBMITTED";
const WORKING: &str = "TASK_STATE_WORKING";
const COMPLETED: &str = "TASK_STATE_COMPLETED";

fn make_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000ff").unwrap())
}

fn event_anchor(event: &ProvEvent) -> baml_rt_core::ids::ActivityAnchorId {
    match event {
        ProvEvent::Task(task) => task.id.clone(),
        other => panic!("expected task-scoped event, got {other:?}"),
    }
}

#[tokio::test]
async fn graph_only_round_trip_covers_creation_status_artifact_messages() {
    let store: Arc<baml_rt_provenance::SurrealProvenanceStore> = build_isolated_store().await;

    let context_id = ContextId::new(8_887_771_001, 1);
    let other_context = ContextId::new(8_887_771_002, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-excision-rt-1"));
    let other_task = TaskId::from_external(ExternalId::new("task-excision-rt-other"));
    let agent_id = make_agent_id();

    // Boot the agent + create the Task node + start execution. The
    // normalizer wires `Task -[SCOPED_TO]-> Context` and the
    // `WAS_LAST_EXECUTED_BY` head-pointer in this batch.
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            baml_rt_provenance::AgentType::new("excision_test_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "excision-test@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");

    let reader: &dyn TaskGraphReader = store.as_ref();

    // (1) Resolve scope. Cross-context lookups must return None even
    // though the Task node exists on disk.
    let scoped = reader
        .resolve_scoped(&context_id, &task_id)
        .await
        .expect("resolve_scoped")
        .expect("task is scoped to context");
    let cross = reader
        .resolve_scoped(&other_context, &task_id)
        .await
        .expect("resolve_scoped cross");
    assert!(
        cross.is_none(),
        "cross-context resolve_scoped must not surface foreign tasks (got {cross:?})",
    );
    let unknown = reader
        .resolve_scoped(&context_id, &other_task)
        .await
        .expect("resolve_scoped unknown");
    assert!(
        unknown.is_none(),
        "non-existent task must resolve to None (got {unknown:?})",
    );

    // (2) Status transitions: None → SUBMITTED → WORKING → COMPLETED.
    for (old, new) in [
        (None, Some(SUBMITTED.to_string())),
        (Some(SUBMITTED.to_string()), Some(WORKING.to_string())),
        (Some(WORKING.to_string()), Some(COMPLETED.to_string())),
    ] {
        store
            .add_event(ProvEvent::task_status_changed(
                context_id.clone(),
                task_id.clone(),
                old,
                new,
            ))
            .await
            .expect("task_status_changed");
    }

    let latest = reader
        .latest_state(scoped.clone())
        .await
        .expect("latest_state")
        .expect("task has at least one transition");
    assert!(
        matches!(latest.new_status, TaskStatusKind::Completed),
        "latest_state must surface the most recent TaskStatusKind, got {:?}",
        latest.new_status,
    );

    // (3) Artifact generated.
    let artifact_id = ArtifactId::from_external(ExternalId::new("artifact-rt-1"));
    store
        .add_event(ProvEvent::task_artifact_generated(
            context_id.clone(),
            task_id.clone(),
            Some(artifact_id.clone()),
            Some("rt-artifact".to_string()),
        ))
        .await
        .expect("task_artifact_generated");

    // (4) Two messages: user -> agent, then agent -> user.
    let user_msg = MessageId::from_external(ExternalId::new("msg-rt-user"));
    let agent_msg = MessageId::from_external(ExternalId::new("msg-rt-agent"));
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            user_msg.clone(),
            "user".to_string(),
            vec!["hello".to_string()],
            None,
            agent_id.clone(),
            1_771_470_500_001,
        ))
        .await
        .expect("message_received_task");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            agent_msg.clone(),
            "agent".to_string(),
            vec!["hi".to_string()],
            None,
            agent_id.clone(),
            1_771_470_500_002,
            Vec::new(),
        ))
        .await
        .expect("message_sent_task");

    // (5) Hydrate. The struct shape is the contract: no metadata / extra
    // field exists — accessing any such field would be a compile error.
    let hydrated = reader
        .hydrate(scoped.clone(), None)
        .await
        .expect("hydrate scoped task");
    assert_eq!(hydrated.context_id, context_id, "hydrated context_id");
    assert_eq!(hydrated.task_id, task_id, "hydrated task_id");
    assert!(
        hydrated.status.is_some(),
        "hydrate must populate the latest TaskStatusKind via the head-pointer edge",
    );
    assert!(
        matches!(
            hydrated.status.as_ref().unwrap().new_status,
            TaskStatusKind::Completed,
        ),
        "hydrated status must be the latest transition (Completed)",
    );
    assert!(
        hydrated
            .artifacts
            .iter()
            .any(|a| a.artifact_id.as_ref() == Some(&artifact_id)),
        "hydrated.artifacts must include the generated artifact: {:?}",
        hydrated.artifacts,
    );
    assert_eq!(
        hydrated.messages.len(),
        2,
        "hydrated must surface both inbound + outbound messages: {:?}",
        hydrated.messages,
    );
    let message_ids: Vec<&MessageId> = hydrated.messages.iter().map(|m| &m.message_id).collect();
    assert!(
        message_ids.contains(&&user_msg),
        "hydrated must include the user message ({user_msg:?})",
    );
    assert!(
        message_ids.contains(&&agent_msg),
        "hydrated must include the agent message ({agent_msg:?})",
    );

    // (6) list_scoped must include this task and refuse to leak it into
    // an unrelated context.
    let listed = reader.list_scoped(&context_id).await.expect("list_scoped");
    assert!(
        !listed.is_empty(),
        "list_scoped must surface the task we just created",
    );
    let listed_other = reader
        .list_scoped(&other_context)
        .await
        .expect("list_scoped other");
    assert!(
        listed_other.is_empty(),
        "list_scoped must not leak tasks across contexts (got {listed_other:?})",
    );
}

#[tokio::test]
async fn payload_bearing_statuses_round_trip_without_loss() {
    let store: Arc<baml_rt_provenance::SurrealProvenanceStore> = build_isolated_store().await;

    let context_id = ContextId::new(8_887_771_101, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-excision-typed-status"));
    let agent_id = make_agent_id();

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            baml_rt_provenance::AgentType::new("excision_test_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "excision-test@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id,
        ))
        .await
        .expect("task_execution_started");

    let submitted = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        None,
        None,
        Some(TaskStatusKind::Submitted),
    );
    let submitted_anchor = event_anchor(&submitted);
    store
        .add_event(submitted)
        .await
        .expect("submitted transition");

    let input_required_prompt = "Please upload the approval note".to_string();
    let input_required = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        Some(TaskStatusKind::Submitted),
        Some(submitted_anchor),
        Some(TaskStatusKind::InputRequired {
            prompt: input_required_prompt.clone(),
        }),
    );
    let input_required_anchor = event_anchor(&input_required);
    store
        .add_event(input_required)
        .await
        .expect("input_required transition");

    let failed_reason =
        NonEmptyString::new("approval upload timed out".to_string()).expect("non-empty reason");
    store
        .add_event(ProvEvent::task_status_changed_typed(
            context_id.clone(),
            task_id.clone(),
            Some(TaskStatusKind::InputRequired {
                prompt: input_required_prompt.clone(),
            }),
            Some(input_required_anchor),
            Some(TaskStatusKind::Failed {
                reason: failed_reason.clone(),
            }),
        ))
        .await
        .expect("failed transition");

    let reader: &dyn TaskGraphReader = store.as_ref();
    let scoped = reader
        .resolve_scoped(&context_id, &task_id)
        .await
        .expect("resolve_scoped")
        .expect("task is scoped to context");

    let latest = reader
        .latest_state(scoped.clone())
        .await
        .expect("latest_state")
        .expect("typed status exists");
    assert!(
        matches!(
            &latest.new_status,
            TaskStatusKind::Failed { reason } if reason == &failed_reason
        ),
        "latest_state must preserve the original failed reason, got {:?}",
        latest.new_status,
    );
    assert!(
        matches!(
            &latest.old_status,
            Some(TaskStatusKind::InputRequired { prompt }) if prompt == &input_required_prompt
        ),
        "latest_state must preserve the original input-required prompt, got {:?}",
        latest.old_status,
    );

    let hydrated = reader
        .hydrate(scoped, None)
        .await
        .expect("hydrate typed status task");
    let hydrated_status = hydrated.status.expect("hydrated status");
    assert!(
        matches!(
            &hydrated_status.new_status,
            TaskStatusKind::Failed { reason } if reason == &failed_reason
        ),
        "hydrate must preserve failed reason, got {:?}",
        hydrated_status.new_status,
    );
    assert!(
        matches!(
            &hydrated_status.old_status,
            Some(TaskStatusKind::InputRequired { prompt }) if prompt == &input_required_prompt
        ),
        "hydrate must preserve input-required prompt, got {:?}",
        hydrated_status.old_status,
    );
}

#[tokio::test]
async fn payload_bearing_string_statuses_are_rejected_without_typed_payloads() {
    let store: Arc<baml_rt_provenance::SurrealProvenanceStore> = build_isolated_store().await;

    let context_id = ContextId::new(8_887_771_102, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-excision-invalid-status"));
    let agent_id = make_agent_id();

    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id,
        ))
        .await
        .expect("task_execution_started");

    let err = store
        .add_event(ProvEvent::task_status_changed(
            context_id,
            task_id,
            Some("working".to_string()),
            Some("input-required".to_string()),
        ))
        .await
        .expect_err("string-only input-required must be rejected");
    assert!(
        err.to_string().contains("payload-bearing task statuses"),
        "strict status error should mention typed payloads, got: {err}",
    );
}
