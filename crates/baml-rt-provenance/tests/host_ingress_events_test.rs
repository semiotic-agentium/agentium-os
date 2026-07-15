// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host ingress `ProvEvent` variants validate, normalize, and project to conversation context.

use std::sync::Arc;

use baml_rt_conversation::view::UserSpeakerKind;
use baml_rt_core::{
    AgentDispatchRoutingKey, AgentInstanceId, AgentPackageName, AgentRouteKey, DispatchTarget,
    DispatchWorkUnit, EventSchemaVersion, EventSourceKind, HostIngressRecorder, ProducedEvent,
    RuntimeScope,
    dispatch_ingress::dispatch_unit_task_id,
    event_subscription::EventSourceKey,
    host_source_records_body::format_source_records_unit_body,
    ids::{ActivityAnchorId, AgentId, ContextId, CorrelationId, MessageId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphExporter, HostIngressRecorderImpl, ProvEvent, ProvenanceContextReader,
    ProvenanceQueryApi, ProvenanceWriter, SurrealProvenanceStore,
    events::ProvEventData,
    graph_model::event_kind_from_data,
    host_ingress_identity::{
        activity_anchor_for_ingress_poll_user, activity_anchor_for_ingress_unit_user,
    },
    host_ingress_types::{HostDispatchFailureKind, HostDispatchRejectedSpec, HostIngressSourceRef},
    normalizer::validate_event,
    vocabulary::a2a,
};
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store;
use uuid::Uuid;

fn test_route(package: &str) -> AgentRouteKey {
    AgentRouteKey::new(
        AgentPackageName::parse(package).expect("package"),
        AgentInstanceId::default(),
    )
}

fn dispatch_target(package: &str, agent_id: AgentId) -> DispatchTarget {
    DispatchTarget::new(test_route(package), agent_id)
}

fn test_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::new_v4()))
}

async fn boot_test_agent(
    store: &SurrealProvenanceStore,
    agent_id: AgentId,
    package: &str,
    archive: &str,
) {
    store
        .add_event(ProvEvent::agent_booted(
            agent_id,
            AgentType::new(package).expect("type"),
            "1.0.0".to_string(),
            archive.to_string(),
        ))
        .await
        .expect("agent_booted");
}

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
        None,
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
            producer_key: None,
            target_agent_id: None,
        },
    });
    validate_event(&event).expect("valid");
    let _ = CorrelationId::new(1, 1);
}

#[tokio::test]
async fn host_ingress_lineage_events_project_operational_transcript_rows() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(10, 20);
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));

    store
        .add_event(ProvEvent::host_source_poll_recorded(
            ctx.clone(),
            "slack".to_string(),
            "slack:C123".to_string(),
            "slack:1:2:1".to_string(),
            "host.source-records.v1".to_string(),
            2,
            None,
            vec!["1".to_string(), "2".to_string()],
        ))
        .await
        .expect("poll write");

    boot_test_agent(
        &store,
        agent_id.clone(),
        "dispatch-echo",
        "dispatch-echo@1.0.0",
    )
    .await;

    store
        .add_event(ProvEvent::host_dispatch_accepted(
            ctx.clone(),
            "event:intake".to_string(),
            "host.source-records.v1".to_string(),
            dispatch_target("dispatch-echo", agent_id),
            "slack".to_string(),
            "slack:C123".to_string(),
            None,
        ))
        .await
        .expect("dispatch write");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        2,
        "poll + dispatch accepted operational rows: {items:?}"
    );
    assert!(items.iter().all(|i| i.role == "host"));
    assert!(
        items.iter().all(|i| matches!(
            i.content,
            baml_rt_conversation::view::ConversationItemContent::Operational(_)
        )),
        "{items:?}"
    );
    assert!(
        items.iter().all(|i| i.timestamp_ms > 0),
        "host ingress operational rows must carry event_order from write time: {items:?}"
    );
    assert!(
        items[0].timestamp_ms <= items[1].timestamp_ms,
        "poll must precede dispatch accepted: {items:?}"
    );
}

#[tokio::test]
async fn record_source_poll_uses_event_source_identity_for_non_source_records_payloads() {
    let store = Arc::new(build_isolated_store().await);
    let recorder = HostIngressRecorderImpl::new(Arc::clone(&store));
    let ctx = ContextId::new(12, 34);
    let event = ProducedEvent {
        routing_key: AgentDispatchRoutingKey::parse("grafana:intake").expect("routing"),
        schema_version: EventSchemaVersion::parse("grafana.alert.v1").expect("schema"),
        source_kind: EventSourceKind::parse("grafana").expect("kind"),
        source_key: EventSourceKey::parse("grafana:local").expect("key"),
        messages: vec![json!({
            "status": "firing",
            "fingerprint": "fp1",
            "labels": {"alertname": "HighLatency"}
        })],
        context_id: Some(ctx.clone()),
        task_id: None,
        message_id: Some("grafana:fp1:firing:start".into()),
        producer_key: None,
        metadata: None,
    };

    recorder
        .record_source_poll(&event)
        .await
        .expect("poll lineage");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 1, "one source poll row: {items:?}");
    let baml_rt_conversation::view::ConversationItemContent::Operational(op) = &items[0].content
    else {
        panic!("expected operational row: {:?}", items[0].content);
    };
    assert!(matches!(
        op.kind,
        baml_rt_conversation::operational::OperationalEventKind::SourcePollRecorded
    ));
    assert!(
        op.summary
            .contains("Host event (grafana.alert.v1) from grafana:grafana:local"),
        "source identity should come from ProducedEvent, got: {}",
        op.summary
    );
}

#[tokio::test]
async fn host_dispatch_rejected_projects_operational_row() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(11, 22);
    store
        .add_event(ProvEvent::host_dispatch_rejected(
            HostDispatchRejectedSpec {
                context_id: ctx.clone(),
                routing_key: "event:intake".to_string(),
                schema_version: EventSchemaVersion::parse("host.source-records.v1")
                    .expect("schema"),
                target: DispatchTarget::with_optional_agent(test_route("coordinator-agent"), None),
                source: HostIngressSourceRef::from_fields("slack", "slack:C123"),
                producer_key: None,
                detail: "no handler for record_kind".to_string(),
                failure_kind: HostDispatchFailureKind::Rejected,
            },
        ))
        .await
        .expect("reject write");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 1);
    let baml_rt_conversation::view::ConversationItemContent::Operational(op) = &items[0].content
    else {
        panic!("expected operational row: {:?}", items[0].content);
    };
    assert!(matches!(
        op.kind,
        baml_rt_conversation::operational::OperationalEventKind::DispatchRejected
    ));
    assert!(op.summary.contains("coordinator-agent"));
}

#[tokio::test]
async fn host_dispatch_rejected_is_idempotent_on_double_write() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(11, 22);
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    boot_test_agent(
        &store,
        agent_id.clone(),
        "coordinator-agent",
        "coordinator-agent@1.0.0",
    )
    .await;
    let reject = ProvEvent::host_dispatch_rejected(HostDispatchRejectedSpec {
        context_id: ctx.clone(),
        routing_key: "event:intake".to_string(),
        schema_version: EventSchemaVersion::parse("host.source-records.v1").expect("schema"),
        target: dispatch_target("coordinator-agent", agent_id),
        source: HostIngressSourceRef::from_fields("slack", "slack:C123"),
        producer_key: None,
        detail: "no handler for record_kind".to_string(),
        failure_kind: HostDispatchFailureKind::Rejected,
    });
    store
        .add_event(reject.clone())
        .await
        .expect("first reject write");
    store.add_event(reject).await.expect("second reject write");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        1,
        "duplicate reject events must upsert one row: {items:?}"
    );

    let exported = GraphExporter::new(store)
        .export_by_context(ctx.as_str())
        .await
        .expect("export");
    let dispatch_edges: Vec<_> = exported
        .edges
        .iter()
        .filter(|e| {
            e.relation == baml_rt_provenance::vocabulary::a2a_relations::HOST_DISPATCH_TARGET
        })
        .collect();
    assert_eq!(
        dispatch_edges.len(),
        1,
        "one dispatch target edge: {dispatch_edges:?}"
    );
}

#[tokio::test]
async fn host_dispatch_accepted_links_booted_runtime_instance() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(99, 88);
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    boot_test_agent(
        &store,
        agent_id.clone(),
        "clickup-agent",
        "clickup-agent@1.0.0",
    )
    .await;

    store
        .add_event(ProvEvent::host_dispatch_accepted(
            ctx.clone(),
            "event:intake".to_string(),
            "host.source-records.v1".to_string(),
            dispatch_target("clickup-agent", agent_id.clone()),
            "clickup".to_string(),
            "clickup:list:1".to_string(),
            None,
        ))
        .await
        .expect("dispatch accepted");

    let exported = GraphExporter::new(store)
        .export_by_context(ctx.as_str())
        .await
        .expect("export");
    let dispatch_edges: Vec<_> = exported
        .edges
        .iter()
        .filter(|e| {
            e.relation == baml_rt_provenance::vocabulary::a2a_relations::HOST_DISPATCH_TARGET
        })
        .collect();
    assert_eq!(dispatch_edges.len(), 1, "{dispatch_edges:?}");
    assert!(
        dispatch_edges[0].to.contains(agent_id.as_str()),
        "HOST_DISPATCH_TARGET must point at live booted instance: {:?}",
        dispatch_edges[0]
    );

    let mermaid = baml_rt_provenance::graph_export::sequence::render_sequence_diagram(&exported);
    let clickup_lifelines = mermaid
        .lines()
        .filter(|line| {
            line.trim_start()
                .starts_with("participant clickup_agent_1_0_0 ")
        })
        .count();
    assert_eq!(
        clickup_lifelines, 1,
        "sequence export must not duplicate clickup lifelines: {mermaid}"
    );
    assert!(
        !mermaid.contains("clickup_agent_default"),
        "route stub participant must not appear: {mermaid}"
    );
}

#[tokio::test]
async fn ingress_poll_user_message_projects_ingress_speaker_kind() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(12, 34);
    let message_id = MessageId::from("poll-msg-1");
    let anchor = activity_anchor_for_ingress_poll_user(&ctx, message_id.as_str());
    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        "user_speaker_kind".to_string(),
        baml_rt_vocabulary::vocabulary::user_speaker_kinds::INGRESS.to_string(),
    );
    store
        .add_event(ProvEvent::Global(baml_rt_provenance::events::GlobalEvent {
            id: anchor,
            context_id: ctx.clone(),
            timestamp_ms: 2,
            data: ProvEventData::MessageReceived {
                id: message_id,
                role: "user".to_string(),
                content: vec!["task from poll".to_string()],
                metadata: Some(metadata),
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
    let store = build_isolated_store().await;
    let ctx = ContextId::new(55, 66);
    let unit_key = "clickup-created:task-a:1";
    let task_id = dispatch_unit_task_id(&ctx, unit_key);

    let unit_anchor = activity_anchor_for_ingress_unit_user(&ctx, unit_key);
    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        "user_speaker_kind".to_string(),
        baml_rt_vocabulary::vocabulary::user_speaker_kinds::INGRESS.to_string(),
    );
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
                    metadata: Some(metadata),
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
    let store = Arc::new(build_isolated_store().await);
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
        producer_key: None,
        metadata: None,
    };
    recorder
        .record_source_poll(&event)
        .await
        .expect("poll lineage");
    let agent_id = test_agent_id();
    boot_test_agent(
        &store,
        agent_id.clone(),
        "clickup-agent",
        "clickup-agent@1.0.0",
    )
    .await;
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
    assert_eq!(
        items.len(),
        2,
        "poll operational row + ingress user line: {items:?}"
    );
    let operational = items
        .iter()
        .find(|i| {
            matches!(
                i.content,
                baml_rt_conversation::view::ConversationItemContent::Operational(_)
            )
        })
        .expect("poll operational row");
    assert_eq!(operational.role, "host");
    let ingress = items
        .iter()
        .find(|i| i.user_speaker_kind == Some(UserSpeakerKind::Ingress))
        .expect("ingress user row");
    let text = match &ingress.content {
        baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => text.as_str(),
        other => panic!("expected message, got {other:?}"),
    };
    assert_eq!(text, expected_body.0);
    assert!(
        operational.timestamp_ms <= ingress.timestamp_ms,
        "poll should precede unit ingress: {items:?}"
    );

    store
        .query_conversation_context(&ctx, None, None, Some("clickup-agent"))
        .await
        .expect("agent_package filter over ingress unit row");
}

#[tokio::test]
async fn host_ingress_dispatch_accepted_precedes_unit_ingress_user() {
    use baml_rt_core::AgentDispatchRequest;

    let store = Arc::new(build_isolated_store().await);
    let recorder = HostIngressRecorderImpl::new(Arc::clone(&store));
    let ctx = ContextId::new(44, 55);
    let unit_key = "clickup-created:dispatch-order:1";
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
            "task_id": "dispatch-order",
            "list_id": "list-1",
            "snapshot": { "name": "Order test" },
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
        message_id: Some("evt-order-msg".into()),
        producer_key: None,
        metadata: None,
    };
    recorder
        .record_source_poll(&event)
        .await
        .expect("poll lineage");
    let agent_id = test_agent_id();
    boot_test_agent(
        &store,
        agent_id.clone(),
        "clickup-agent",
        "clickup-agent@1.0.0",
    )
    .await;
    let dispatch_request = AgentDispatchRequest {
        routing_key: AgentDispatchRoutingKey::parse("event:intake").expect("routing"),
        message_type: EventSchemaVersion::parse("host.source-records.v1").expect("schema"),
        messages: vec![batch],
        context_id: Some(ctx.clone()),
        task_id: None,
        message_id: Some("evt-order-msg".into()),
        source_kind: Some(EventSourceKind::parse("clickup").expect("kind")),
        source_key: Some(EventSourceKey::parse("clickup:list:1").expect("key")),
        producer_key: Some("test-clickup-producer".to_string()),
        metadata: None,
    };
    recorder
        .record_dispatch_accepted(
            &dispatch_request,
            dispatch_target("clickup-agent", agent_id.clone()),
        )
        .await
        .expect("dispatch accepted");
    let parent =
        RuntimeScope::message_scope(ctx.clone(), agent_id.clone(), MessageId::from("parent-msg"));
    let records: Vec<serde_json::Value> = event
        .messages
        .first()
        .and_then(|v| v.get("records"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let unit = DispatchWorkUnit::new(unit_key.to_string(), records).expect("unit");
    recorder
        .with_task_prelude(&parent, agent_id, unit)
        .await
        .expect("unit prelude");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        3,
        "poll + dispatch accepted + ingress user line: {items:?}"
    );
    assert!(
        items.iter().all(|i| i.timestamp_ms > 0),
        "all host rows must carry event_order: {items:?}"
    );
    let poll = items
        .iter()
        .find(|i| {
            matches!(
                &i.content,
                baml_rt_conversation::view::ConversationItemContent::Operational(op)
                    if matches!(
                        op.kind,
                        baml_rt_conversation::operational::OperationalEventKind::SourcePollRecorded
                    )
            )
        })
        .expect("poll row");
    let dispatch = items
        .iter()
        .find(|i| {
            matches!(
                &i.content,
                baml_rt_conversation::view::ConversationItemContent::Operational(op)
                    if matches!(
                        op.kind,
                        baml_rt_conversation::operational::OperationalEventKind::DispatchAccepted
                    )
            )
        })
        .expect("dispatch row");
    let ingress = items
        .iter()
        .find(|i| i.user_speaker_kind == Some(UserSpeakerKind::Ingress))
        .expect("ingress user row");
    assert!(
        poll.timestamp_ms <= dispatch.timestamp_ms && dispatch.timestamp_ms <= ingress.timestamp_ms,
        "causal host order poll → dispatch → unit ingress: {items:?}"
    );
}

#[tokio::test]
async fn transcript_engine_includes_event_order_zero_ingress_with_later_index_rows() {
    use baml_rt_core::Outcome;
    use baml_rt_provenance::{
        ObservationScope, TaskObservationScope, TemporalBound, TranscriptEngine,
        TranscriptPageRequest, TranscriptProjectionProfile,
    };

    let store = Arc::new(build_isolated_store().await);
    let recorder = HostIngressRecorderImpl::new(Arc::clone(&store));
    let ctx = ContextId::new(111, 222);
    let unit_key = "clickup-created:restore-ingress:1";
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
            "task_id": "restore-ingress",
            "list_id": "list-1",
            "snapshot": { "name": "Restore ingress balloon", "description": "wire body" },
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
        message_id: Some("evt-restore-msg".into()),
        producer_key: None,
        metadata: None,
    };
    recorder
        .record_source_poll(&event)
        .await
        .expect("poll lineage");
    let agent_id = test_agent_id();
    boot_test_agent(
        &store,
        agent_id.clone(),
        "clickup-agent",
        "clickup-agent@1.0.0",
    )
    .await;
    let parent =
        RuntimeScope::message_scope(ctx.clone(), agent_id.clone(), MessageId::from("parent-msg"));
    let records: Vec<serde_json::Value> = batch
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let unit = DispatchWorkUnit::new(unit_key.to_string(), records).expect("unit");
    recorder
        .with_task_prelude(&parent, agent_id.clone(), unit)
        .await
        .expect("unit prelude");
    let task_id = dispatch_unit_task_id(&ctx, unit_key);
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            Some("list_teams".to_string()),
            json!({}),
            json!({ "agent_id": agent_id.as_str() }),
            50,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool");

    let page = TranscriptEngine::page(
        store.as_ref() as &SurrealProvenanceStore,
        TranscriptPageRequest {
            scope: ObservationScope {
                context_id: ctx.clone(),
                task: TaskObservationScope::ContextWide,
                agent_package: Some("clickup-agent".to_string()),
                temporal: TemporalBound::All,
            },
            limit: 50,
            profile: TranscriptProjectionProfile::OperatorTimeline,
        },
    )
    .await
    .expect("transcript page");

    assert!(
        page.items
            .iter()
            .any(|i| i.user_speaker_kind == Some(UserSpeakerKind::Ingress)),
        "host ingress user row must appear when later tool rows populate the index: {:?}",
        page.items
    );
    let ingress = page
        .items
        .iter()
        .find(|i| i.user_speaker_kind == Some(UserSpeakerKind::Ingress))
        .expect("ingress row");
    assert!(
        ingress.timestamp_ms > 0,
        "ingress user row must carry event_order from write time: {:?}",
        page.items
    );
}

#[tokio::test]
async fn with_task_prelude_is_idempotent_for_same_unit_key() {
    let store = Arc::new(build_isolated_store().await);
    let recorder = HostIngressRecorderImpl::new(Arc::clone(&store));
    let ctx = ContextId::new(88, 99);
    let unit_key = "slack:C012:1735720111.000001";
    let agent_id = test_agent_id();
    boot_test_agent(&store, agent_id.clone(), "slack-agent", "slack-agent@1.0.0").await;
    let parent =
        RuntimeScope::message_scope(ctx.clone(), agent_id.clone(), MessageId::from("parent-msg"));
    let records = vec![json!({
        "channel_id": "C012TEST001",
        "ts": "1735720111.000001",
        "text": "Please turn this Slack thread into a tracked task."
    })];
    let unit = DispatchWorkUnit::new(unit_key.to_string(), records.clone()).expect("unit");
    recorder
        .with_task_prelude(&parent, agent_id.clone(), unit.clone())
        .await
        .expect("first prelude");
    recorder
        .with_task_prelude(&parent, agent_id, unit)
        .await
        .expect("second prelude");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        1,
        "duplicate withTask prelude must upsert one ingress user row: {items:?}"
    );
}

#[tokio::test]
async fn transport_and_agent_reject_are_distinct_operational_rows() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(21, 22);
    let source = HostIngressSourceRef::from_fields("slack", "slack:C123");
    store
        .add_event(ProvEvent::host_dispatch_rejected(
            HostDispatchRejectedSpec {
                context_id: ctx.clone(),
                routing_key: "event:intake".to_string(),
                schema_version: EventSchemaVersion::parse("host.source-records.v1")
                    .expect("schema"),
                target: DispatchTarget::with_optional_agent(test_route("slack-agent"), None),
                source: source.clone(),
                producer_key: None,
                detail: "transport failed".to_string(),
                failure_kind: HostDispatchFailureKind::TransportError,
            },
        ))
        .await
        .expect("transport reject");
    store
        .add_event(ProvEvent::host_dispatch_rejected(
            HostDispatchRejectedSpec {
                context_id: ctx.clone(),
                routing_key: "event:intake".to_string(),
                schema_version: EventSchemaVersion::parse("host.source-records.v1")
                    .expect("schema"),
                target: DispatchTarget::with_optional_agent(test_route("slack-agent"), None),
                source,
                producer_key: None,
                detail: "agent rejected".to_string(),
                failure_kind: HostDispatchFailureKind::Rejected,
            },
        ))
        .await
        .expect("agent reject");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        2,
        "transport vs agent reject are distinct outcome keys: {items:?}"
    );
}

#[tokio::test]
async fn host_source_poll_is_idempotent_on_double_write() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(40, 41);
    let poll = ProvEvent::host_source_poll_recorded(
        ctx.clone(),
        "clickup".to_string(),
        "clickup:list:901325431486".to_string(),
        "clickup:list:901325431486".to_string(),
        "host.source-records.v1".to_string(),
        1,
        None,
        vec![],
    );
    store.add_event(poll.clone()).await.expect("first poll");
    store.add_event(poll).await.expect("second poll");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    let poll_rows: Vec<_> = items
        .iter()
        .filter(|i| {
            matches!(
                &i.content,
                baml_rt_conversation::view::ConversationItemContent::Operational(op)
                    if matches!(
                        op.kind,
                        baml_rt_conversation::operational::OperationalEventKind::SourcePollRecorded
                    )
            )
        })
        .collect();
    assert_eq!(
        poll_rows.len(),
        1,
        "duplicate poll events must upsert one row: {items:?}"
    );
}

#[tokio::test]
async fn prompt_rejected_persists_when_emitted_after_llm_completed_separate_add_event() {
    use baml_rt_core::Outcome;
    use baml_rt_provenance::{
        CallScope,
        events::{GlobalEvent, LlmUsage},
    };

    let store = build_isolated_store().await;
    let ctx = ContextId::new(50, 51);
    let msg = MessageId::from("msg-llm-fail");
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::nil()));
    let mut meta = serde_json::Map::new();
    meta.insert(
        "agent_id".to_string(),
        serde_json::Value::String(agent_id.as_str().to_string()),
    );
    meta.insert(
        "message_id".to_string(),
        serde_json::Value::String(msg.as_str().to_string()),
    );
    let llm_anchor = ActivityAnchorId::from("llm-fail-anchor-1");
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: llm_anchor.clone(),
            context_id: ctx.clone(),
            timestamp_ms: 10,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message {
                    message_id: msg.clone(),
                },
                client: "openai".to_string(),
                model: "gpt-test".to_string(),
                function_name: "TestFn".to_string(),
                prompt: serde_json::json!({"q":"x"}),
                metadata: serde_json::Value::Object(meta),
                usage: LlmUsage::Known {
                    prompt_tokens: 1,
                    completion_tokens: 0,
                    total_tokens: 1,
                    cached_input_tokens: None,
                },
                duration_ms: 100,
                outcome: Outcome::Failure,
                citation_integrity: None,
                citations: vec![],
                resolved_citations: vec![],
                prompt_serialized_utf8_bytes: 2,
                prompt_message_chars: 1,
            },
        }))
        .await
        .expect("llm completed");
    store
        .add_event(ProvEvent::prompt_rejected_global(
            ctx.clone(),
            msg,
            llm_anchor,
            "Runtime configuration error: CLICKUP_API_KEY not resolved".to_string(),
        ))
        .await
        .expect("prompt rejected");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation_context");
    assert!(
        items.iter().any(|i| matches!(
            &i.content,
            baml_rt_conversation::view::ConversationItemContent::Operational(op)
                if matches!(
                    op.kind,
                    baml_rt_conversation::operational::OperationalEventKind::PromptRejected
                )
        )),
        "expected prompt_rejected operational row: {items:?}"
    );
}

#[tokio::test]
async fn task_scoped_conversation_history_includes_llm_call_failed() {
    use baml_rt_core::Outcome;
    use baml_rt_provenance::{
        CallScope,
        events::{LlmUsage, TaskScopedEvent},
    };

    let store = build_isolated_store().await;
    let ctx = ContextId::new(60, 61);
    let task_id = dispatch_unit_task_id(&ctx, "unit-fail-a");
    let other_task_id = dispatch_unit_task_id(&ctx, "unit-fail-b");
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::nil()));
    let llm_anchor = ActivityAnchorId::from("llm-task-fail-anchor");
    let other_llm_anchor = ActivityAnchorId::from("llm-other-task-fail-anchor");

    let task_llm_meta = serde_json::json!({
        "agent_id": agent_id.as_str(),
        "task_id": task_id.as_str(),
    });
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: llm_anchor.clone(),
            context_id: ctx.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 10,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Task {
                    task_id: task_id.clone(),
                },
                client: "openai".to_string(),
                model: "gpt-test".to_string(),
                function_name: "DispatchFn".to_string(),
                prompt: serde_json::json!({"q":"x"}),
                metadata: task_llm_meta,
                usage: LlmUsage::Known {
                    prompt_tokens: 1,
                    completion_tokens: 0,
                    total_tokens: 1,
                    cached_input_tokens: None,
                },
                duration_ms: 100,
                outcome: Outcome::Failure,
                citation_integrity: None,
                citations: vec![],
                resolved_citations: vec![],
                prompt_serialized_utf8_bytes: 2,
                prompt_message_chars: 1,
            },
        }))
        .await
        .expect("task llm failed");

    let other_meta = serde_json::json!({
        "agent_id": agent_id.as_str(),
        "task_id": other_task_id.as_str(),
    });
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: other_llm_anchor,
            context_id: ctx.clone(),
            task_id: other_task_id.clone(),
            timestamp_ms: 20,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Task {
                    task_id: other_task_id.clone(),
                },
                client: "openai".to_string(),
                model: "gpt-test".to_string(),
                function_name: "OtherFn".to_string(),
                prompt: serde_json::json!({"q":"y"}),
                metadata: other_meta,
                usage: LlmUsage::Known {
                    prompt_tokens: 1,
                    completion_tokens: 0,
                    total_tokens: 1,
                    cached_input_tokens: None,
                },
                duration_ms: 50,
                outcome: Outcome::Failure,
                citation_integrity: None,
                citations: vec![],
                resolved_citations: vec![],
                prompt_serialized_utf8_bytes: 2,
                prompt_message_chars: 1,
            },
        }))
        .await
        .expect("other task llm failed");

    let task_items = store
        .conversation_context_with_task(&ctx, None, Some(&task_id))
        .await
        .expect("task-scoped context");
    assert!(
        task_items.iter().any(|i| matches!(
            &i.content,
            baml_rt_conversation::view::ConversationItemContent::Operational(op)
                if matches!(
                    op.kind,
                    baml_rt_conversation::operational::OperationalEventKind::LlmCallFailed
                )
        )),
        "task-scoped history must include llm_call_failed: {task_items:?}"
    );
    assert!(
        !task_items.iter().any(|i| matches!(
            &i.content,
            baml_rt_conversation::view::ConversationItemContent::Operational(op)
                if op.summary.contains("OtherFn")
        )),
        "task filter must exclude other task failures: {task_items:?}"
    );
}
