//! Host ingress `ProvEvent` variants validate, normalize, and project to conversation context.

use baml_rt_conversation::view::UserSpeakerKind;
use baml_rt_core::{
    dispatch_ingress::dispatch_unit_task_id,
    ids::{ActivityAnchorId, AgentId, ContextId, CorrelationId, MessageId, UuidId},
};
use baml_rt_provenance::{
    GraphExporter, ProvEvent, ProvenanceContextReader, ProvenanceWriter, SurrealStoreBuilder,
    events::ProvEventData, graph_model::event_kind_from_data, normalizer::validate_event,
    vocabulary::a2a,
};
use uuid::Uuid;

#[test]
fn host_source_poll_recorded_kind_and_validation() {
    let ctx = ContextId::new(1, 2);
    let event = ProvEvent::host_source_poll_recorded(
        ctx.clone(),
        "slack".to_string(),
        "slack:C123".to_string(),
        "slack:1:2:1".to_string(),
        "host.source-records.v1".to_string(),
        1,
        vec!["1".to_string()],
    );
    assert!(matches!(
        event.data(),
        ProvEventData::HostSourcePollRecorded {
            record_count: 1,
            ..
        }
    ));
    assert_eq!(
        event_kind_from_data(event.data()),
        baml_rt_provenance::graph_model::EventGraphKind::HostSourcePollRecorded
    );
    validate_event(&event).expect("valid");
    assert_eq!(event.context_id(), &ctx);
}

#[test]
fn host_dispatch_accepted_requires_context() {
    let event = ProvEvent::Global(baml_rt_provenance::events::GlobalEvent {
        id: baml_rt_core::ActivityAnchorId::from("a1".to_string()),
        context_id: ContextId::new(3, 4),
        timestamp_ms: 1,
        data: ProvEventData::HostDispatchAccepted {
            routing_key: "event:intake".to_string(),
            schema_version: "host.source-records.v1".to_string(),
            target_package: "slack-agent".to_string(),
            target_instance: "default".to_string(),
            source_kind: "slack".to_string(),
            source_key: "slack:C123".to_string(),
        },
    });
    validate_event(&event).expect("valid");
    let _ = CorrelationId::new(1, 1);
}

#[tokio::test]
async fn host_ingress_lineage_events_do_not_project_bookkeeping_transcript_rows() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let ctx = ContextId::new(10, 20);

    store
        .add_event(ProvEvent::host_source_poll_recorded(
            ctx.clone(),
            "slack".to_string(),
            "slack:C123".to_string(),
            "slack:1:2:1".to_string(),
            "host.source-records.v1".to_string(),
            2,
            vec!["1".to_string(), "2".to_string()],
        ))
        .await
        .expect("poll write");

    store
        .add_event(ProvEvent::host_dispatch_accepted(
            ctx.clone(),
            "event:intake".to_string(),
            "host.source-records.v1".to_string(),
            "dispatch-echo".to_string(),
            "default".to_string(),
            "slack".to_string(),
            "slack:C123".to_string(),
        ))
        .await
        .expect("dispatch write");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert!(
        items.is_empty(),
        "poll/dispatch lineage events must not emit host-role transcript rows: {items:?}"
    );
}

#[tokio::test]
async fn ingress_poll_user_message_projects_ingress_speaker_kind() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let ctx = ContextId::new(12, 34);
    let message_id = MessageId::from("poll-msg-1");
    let anchor = ActivityAnchorId::from(format!(
        "ingress-poll-user:{}:{}",
        ctx.as_str(),
        message_id.as_str()
    ));
    store
        .add_event(ProvEvent::Global(baml_rt_provenance::events::GlobalEvent {
            id: anchor,
            context_id: ctx.clone(),
            timestamp_ms: 2,
            data: ProvEventData::MessageReceived {
                id: message_id,
                role: "user".to_string(),
                content: vec!["task from poll".to_string()],
                metadata: None,
                agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
                citations: Vec::new(),
            },
        }))
        .await
        .expect("poll user message");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].role, "user");
    assert_eq!(items[0].user_speaker_kind, Some(UserSpeakerKind::Ingress));

    let exported = GraphExporter::new(store)
        .export_by_context(ctx.as_str())
        .await
        .expect("export");
    let message_nodes: Vec<_> = exported
        .nodes
        .iter()
        .filter(|n| n.label == "Message")
        .collect();
    assert_eq!(message_nodes.len(), 1);
    assert_eq!(
        message_nodes[0]
            .properties
            .get(a2a::USER_SPEAKER_KIND)
            .and_then(|v| v.as_str()),
        Some("ingress")
    );
}

#[tokio::test]
async fn conversation_context_with_task_omits_global_poll_user_line() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let ctx = ContextId::new(55, 66);
    let unit_key = "clickup-created:task-a:1";
    let task_id = dispatch_unit_task_id(&ctx, unit_key);

    let poll_message_id = MessageId::from("poll-msg-batch");
    let poll_anchor = ActivityAnchorId::from(format!(
        "ingress-poll-user:{}:{}",
        ctx.as_str(),
        poll_message_id.as_str()
    ));
    store
        .add_event(ProvEvent::Global(baml_rt_provenance::events::GlobalEvent {
            id: poll_anchor,
            context_id: ctx.clone(),
            timestamp_ms: 1,
            data: ProvEventData::MessageReceived {
                id: poll_message_id,
                role: "user".to_string(),
                content: vec!["1. Poll batch line".to_string()],
                metadata: None,
                agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
                citations: Vec::new(),
            },
        }))
        .await
        .expect("poll user");

    let unit_anchor =
        ActivityAnchorId::from(format!("ingress-unit-user:{}:{}", ctx.as_str(), unit_key));
    store
        .add_event(ProvEvent::Task(
            baml_rt_provenance::events::TaskScopedEvent {
                id: unit_anchor,
                context_id: ctx.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 2,
                data: ProvEventData::MessageReceived {
                    id: MessageId::from("unit-msg-a"),
                    role: "user".to_string(),
                    content: vec!["1. Unit task line".to_string()],
                    metadata: None,
                    agent_id: AgentId::from_uuid(UuidId::new(Uuid::nil())),
                    citations: Vec::new(),
                },
            },
        ))
        .await
        .expect("unit user");

    let full = store
        .conversation_context(&ctx, None)
        .await
        .expect("full context");
    assert_eq!(full.len(), 2, "full context includes poll + unit: {full:?}");

    let task_only = store
        .conversation_context_with_task(&ctx, None, Some(&task_id))
        .await
        .expect("task context");
    assert_eq!(
        task_only.len(),
        1,
        "task filter must drop global poll: {task_only:?}"
    );
    let text = match &task_only[0].content {
        baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => text.as_str(),
        other => panic!("expected message content, got {other:?}"),
    };
    assert!(
        text.contains("Unit task line"),
        "task-scoped read must be the unit ingress line only: {text}"
    );
}
