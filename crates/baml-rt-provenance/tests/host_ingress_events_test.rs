//! Host ingress `ProvEvent` variants validate, normalize, and project to conversation context.

use std::sync::Arc;

use baml_rt_conversation::view::UserSpeakerKind;
use baml_rt_core::{
    AgentDispatchRoutingKey, DispatchWorkUnit, EventSchemaVersion, EventSourceKind,
    HostIngressRecorder, ProducedEvent, RuntimeScope,
    dispatch_ingress::dispatch_unit_task_id,
    event_subscription::EventSourceKey,
    host_source_records_body::format_source_records_unit_body,
    ids::{ActivityAnchorId, AgentId, ContextId, CorrelationId, MessageId, UuidId},
};
use baml_rt_provenance::{
    GraphExporter, HostIngressRecorderImpl, ProvEvent, ProvenanceContextReader, ProvenanceQueryApi,
    ProvenanceWriter, SurrealStoreBuilder, events::ProvEventData,
    graph_model::event_kind_from_data, normalizer::validate_event, vocabulary::a2a,
};
use serde_json::json;
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
async fn conversation_context_unit_ingress_only_without_poll_user_line() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let ctx = ContextId::new(55, 66);
    let unit_key = "clickup-created:task-a:1";
    let task_id = dispatch_unit_task_id(&ctx, unit_key);

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
                    content: vec![
                        format_source_records_unit_body(&[json!({
                            "record_kind": "clickup.lifecycle_event",
                            "key": unit_key,
                            "event": "created",
                            "task_id": "task-a",
                            "list_id": "list-1",
                            "snapshot": { "name": "Unit task line" },
                            "revision": 1
                        })])
                        .0,
                    ],
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
    assert_eq!(full.len(), 1, "canonical ingress is unit-only: {full:?}");

    let task_only = store
        .conversation_context_with_task(&ctx, None, Some(&task_id))
        .await
        .expect("task context");
    assert_eq!(
        task_only.len(),
        1,
        "task-scoped matches full: {task_only:?}"
    );
    let text = match &task_only[0].content {
        baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => text.as_str(),
        other => panic!("expected message content, got {other:?}"),
    };
    assert!(
        text.contains("clickup.lifecycle_event"),
        "ingress line must be wire JSON prelude: {text}"
    );
    assert!(text.contains("Unit task line"));
}

#[tokio::test]
async fn record_source_poll_and_unit_prelude_emit_single_ingress_user_line() {
    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("store"),
    );
    let recorder = HostIngressRecorderImpl::new(Arc::clone(&store));
    let ctx = ContextId::new(77, 88);
    let unit_key = "clickup-created:task-1:1";
    let batch = json!({
        "schema_version": "host.source-records.v1",
        "source": {
            "source_kind": "clickup",
            "source_key": "clickup:list:1",
            "source_label": "List"
        },
        "records": [{
            "record_kind": "clickup.lifecycle_event",
            "key": unit_key,
            "event": "created",
            "task_id": "task-1",
            "list_id": "list-1",
            "snapshot": { "name": "Investigate ingress", "description": "Confirm single user line" },
            "revision": 1
        }]
    });
    let event = ProducedEvent {
        routing_key: AgentDispatchRoutingKey::parse("event:intake").expect("routing"),
        schema_version: EventSchemaVersion::parse("host.source-records.v1").expect("schema"),
        source_kind: EventSourceKind::parse("clickup").expect("kind"),
        source_key: EventSourceKey::parse("clickup:list:1").expect("key"),
        messages: vec![batch.clone()],
        context_id: Some(ctx.clone()),
        task_id: None,
        message_id: Some("evt-console-msg-1".into()),
        metadata: None,
    };
    recorder
        .record_source_poll(&event)
        .await
        .expect("poll lineage");
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::nil()));
    let parent =
        RuntimeScope::message_scope(ctx.clone(), agent_id.clone(), MessageId::from("parent-msg"));
    let records: Vec<serde_json::Value> = batch
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let expected_body = format_source_records_unit_body(&records);
    let unit = DispatchWorkUnit::new(unit_key.to_string(), records).expect("unit");
    recorder
        .with_task_prelude(&parent, agent_id, unit)
        .await
        .expect("unit prelude");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 1, "one ingress user line only: {items:?}");
    assert_eq!(items[0].user_speaker_kind, Some(UserSpeakerKind::Ingress));
    let text = match &items[0].content {
        baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => text.as_str(),
        other => panic!("expected message, got {other:?}"),
    };
    assert_eq!(text, expected_body.0);

    store
        .query_conversation_context(&ctx, None, None, Some("clickup-agent"))
        .await
        .expect("agent_package filter over ingress unit row");
}
