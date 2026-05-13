use std::sync::Arc;

use baml_rt_core::ids::{AgentId, ArtifactId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceWriter, SurrealStoreBuilder, TaskGraphReader, TaskReplayCursor,
    TaskReplayEvent, metamodel::TaskStatusKind,
};
use futures_util::StreamExt;

fn make_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-0000000000aa").unwrap())
}

fn event_cursor(event: &ProvEvent) -> TaskReplayCursor {
    let anchor = match event {
        ProvEvent::Task(task) => task.id.clone(),
        other => panic!("expected task-scoped event, got {other:?}"),
    };
    TaskReplayCursor::from_anchor(anchor).expect("prov-* cursor")
}

async fn collect_replay(
    reader: &dyn TaskGraphReader,
    scoped: baml_rt_provenance::metamodel::ScopedTaskRef,
    since: Option<TaskReplayCursor>,
) -> Vec<TaskReplayEvent> {
    let mut stream = reader
        .replay_since(scoped, since)
        .await
        .expect("open replay stream");
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.expect("replay event"));
    }
    out
}

#[tokio::test]
async fn replay_since_is_exact_directional_and_preserves_same_status_repeats() {
    let store: Arc<baml_rt_provenance::SurrealProvenanceStore> =
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build isolated in-memory store");
    let reader: &dyn TaskGraphReader = store.as_ref();

    let context_id = ContextId::new(8_888_000_001, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-replay-1"));
    let agent_id = make_agent_id();

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("task_replay_test_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "task-replay-test@1.0.0".to_string(),
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

    let submitted_event = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        None,
        None,
        Some(TaskStatusKind::Submitted),
    );
    let submitted_cursor = event_cursor(&submitted_event);
    store
        .add_event(submitted_event)
        .await
        .expect("status submitted");

    let inbound_message_id = MessageId::from_external(ExternalId::new("msg-replay-in"));
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            inbound_message_id.clone(),
            "user".to_string(),
            vec!["hello replay".to_string()],
            None,
            agent_id.clone(),
            1_771_470_700_001,
        ))
        .await
        .expect("message_received_task");

    let input_required_event = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        Some(TaskStatusKind::Submitted),
        Some(submitted_cursor.anchor().clone()),
        Some(TaskStatusKind::InputRequired {
            prompt: "Need more detail".to_string(),
        }),
    );
    let input_required_cursor = event_cursor(&input_required_event);
    store
        .add_event(input_required_event)
        .await
        .expect("status input_required");

    store
        .add_event(ProvEvent::task_artifact_generated(
            context_id.clone(),
            task_id.clone(),
            Some(ArtifactId::from_external(ExternalId::new(
                "artifact-replay-1",
            ))),
            Some("application/json".to_string()),
        ))
        .await
        .expect("artifact generated");

    let outbound_message_id = MessageId::from_external(ExternalId::new("msg-replay-out"));
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            outbound_message_id.clone(),
            "agent".to_string(),
            vec!["acknowledged".to_string()],
            None,
            agent_id.clone(),
            1_771_470_700_002,
            Vec::new(),
        ))
        .await
        .expect("message_sent_task");

    let working_one_event = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        Some(TaskStatusKind::InputRequired {
            prompt: "Need more detail".to_string(),
        }),
        Some(input_required_cursor.anchor().clone()),
        Some(TaskStatusKind::Working),
    );
    let working_one_cursor = event_cursor(&working_one_event);
    store
        .add_event(working_one_event)
        .await
        .expect("status working one");

    let working_two_event = ProvEvent::task_status_changed_typed(
        context_id.clone(),
        task_id.clone(),
        Some(TaskStatusKind::Working),
        Some(working_one_cursor.anchor().clone()),
        Some(TaskStatusKind::Working),
    );
    store
        .add_event(working_two_event)
        .await
        .expect("status working two");

    let scoped = reader
        .resolve_scoped(&context_id, &task_id)
        .await
        .expect("resolve_scoped")
        .expect("scoped task");

    let replay = collect_replay(reader, scoped.clone(), None).await;
    assert_eq!(
        replay.len(),
        7,
        "expected every immutable fact in replay order"
    );

    assert!(matches!(
        &replay[0],
        TaskReplayEvent::StatusTransition { state, .. }
            if matches!(state.new_status, TaskStatusKind::Submitted)
    ));
    assert!(matches!(
        &replay[1],
        TaskReplayEvent::MessageReceived { message, .. }
            if message.message_id == inbound_message_id
    ));
    assert!(matches!(
        &replay[2],
        TaskReplayEvent::StatusTransition { state, .. }
            if matches!(
                &state.new_status,
                TaskStatusKind::InputRequired { prompt } if prompt == "Need more detail"
            )
    ));
    assert!(matches!(
        &replay[3],
        TaskReplayEvent::ArtifactGenerated { .. }
    ));
    assert!(matches!(
        &replay[4],
        TaskReplayEvent::MessageSent { message, .. }
            if message.message_id == outbound_message_id
    ));

    let working_replays: Vec<_> = replay
        .iter()
        .filter_map(|event| match event {
            TaskReplayEvent::StatusTransition { state, cursor }
                if matches!(state.new_status, TaskStatusKind::Working) =>
            {
                Some(cursor.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        working_replays.len(),
        2,
        "same-status repeats must stay distinct"
    );
    assert_ne!(
        working_replays[0], working_replays[1],
        "working transitions must be anchor-keyed, not collapsed by status"
    );

    let replay_after_input = collect_replay(reader, scoped, Some(input_required_cursor)).await;
    assert_eq!(
        replay_after_input.len(),
        4,
        "cursor window must resume strictly after the supplied cursor"
    );
    assert!(matches!(
        &replay_after_input[0],
        TaskReplayEvent::ArtifactGenerated { .. }
    ));
    assert!(matches!(
        &replay_after_input[1],
        TaskReplayEvent::MessageSent { .. }
    ));
    assert!(matches!(
        &replay_after_input[2],
        TaskReplayEvent::StatusTransition { state, .. }
            if matches!(state.new_status, TaskStatusKind::Working)
    ));
    assert!(matches!(
        &replay_after_input[3],
        TaskReplayEvent::StatusTransition { state, .. }
            if matches!(state.new_status, TaskStatusKind::Working)
    ));
}
