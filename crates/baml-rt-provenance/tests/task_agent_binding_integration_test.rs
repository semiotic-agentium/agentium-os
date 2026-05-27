//! Integration tests for task executing-agent binding (`WAS_LAST_EXECUTED_BY` head pointer).

use std::sync::Arc;

use baml_rt_core::{
    HostIngressRecorder, RuntimeScope,
    dispatch_ingress::{DispatchWorkUnit, dispatch_unit_task_id},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, EpisodeReader, HostIngressRecorderImpl, ProvEvent, ProvenanceError,
    ProvenanceWriter, SurrealStoreBuilder,
    metamodel::{EdgeProjection, SemanticEdge},
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    },
    task_agent_binding::is_unassigned_executing_agent,
};
use serde_json::json;
use uuid::Uuid;

fn test_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::new_v4()))
}

async fn boot_agent(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    agent_id: AgentId,
    package: &str,
) {
    store
        .add_event(ProvEvent::agent_booted(
            agent_id,
            AgentType::new(package).expect("type"),
            "1.0.0".to_string(),
            format!("{package}@1.0.0"),
        ))
        .await
        .expect("agent_booted");
}

async fn head_executed_by_rows(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    task_id: &TaskId,
) -> Vec<serde_json::Value> {
    let task_node = baml_rt_provenance::task_entity_id_string(task_id);
    let (sql, binds) = EdgeProjection::for_edge(SemanticEdge::WasLastExecutedBy)
        .from_id_in(&[task_node])
        .into_surreal();
    let mut q = store.db().query(sql);
    if let Some(obj) = binds.as_object() {
        for (k, v) in obj {
            q = q.bind((k.clone(), v.clone()));
        }
    }
    let mut response = q.await.expect("head pointer query");
    response.take(0).expect("rows")
}

#[tokio::test]
async fn with_task_prelude_binds_executing_agent_head_pointer() {
    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("store"),
    );
    let agent_id = test_agent_id();
    boot_agent(&store, agent_id.clone(), "clickup-agent").await;

    let ctx = ContextId::new(77, 88);
    let unit_key = "clickup-created:task-1:1";
    let parent =
        RuntimeScope::message_scope(ctx.clone(), agent_id.clone(), MessageId::from("parent-msg"));
    let unit = DispatchWorkUnit::new(
        unit_key.to_string(),
        vec![json!({"record_kind": "clickup.lifecycle_event", "key": unit_key})],
    )
    .expect("unit");
    let recorder = HostIngressRecorderImpl::new(Arc::clone(&store));
    recorder
        .with_task_prelude(&parent, agent_id.clone(), unit)
        .await
        .expect("with_task_prelude");

    let task_id = dispatch_unit_task_id(&ctx, unit_key);
    let head_rows = head_executed_by_rows(&store, &task_id).await;
    assert_eq!(
        head_rows.len(),
        1,
        "expected one WAS_LAST_EXECUTED_BY edge: {head_rows:?}"
    );

    let reader = EpisodeReader::new(Arc::clone(&store));
    let episode = reader.read_snapshot(&ctx, &task_id).await.expect("episode");
    assert_eq!(
        episode.agent_id, agent_id,
        "dispatch-unit episode must resolve executing agent"
    );
}

#[tokio::test]
async fn poll_task_scoped_nil_agent_does_not_bind_head_pointer() {
    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("store"),
    );
    let ctx = ContextId::new(1, 2);
    let task_id = TaskId::from_external(ExternalId::new("poll-task".to_string()));
    let nil_agent = AgentId::from_uuid(UuidId::new(Uuid::nil()));
    assert!(is_unassigned_executing_agent(&nil_agent));

    store
        .add_event(ProvEvent::Task(
            baml_rt_provenance::events::TaskScopedEvent {
                id: baml_rt_provenance::events::allocate_activity_anchor(),
                context_id: ctx.clone(),
                task_id: task_id.clone(),
                timestamp_ms: 1,
                data: baml_rt_provenance::events::ProvEventData::MessageReceived {
                    id: MessageId::from("poll-batch-msg"),
                    role: "user".to_string(),
                    content: vec!["poll row".to_string()],
                    metadata: Some(std::collections::HashMap::from([(
                        "user_speaker_kind".to_string(),
                        "ingress".to_string(),
                    )])),
                    agent_id: nil_agent,
                    citations: vec![],
                },
            },
        ))
        .await
        .expect("poll message");

    let head_rows = head_executed_by_rows(&store, &task_id).await;
    assert!(
        head_rows.is_empty(),
        "poll ingress must not bind executing agent head: {head_rows:?}"
    );

    let reader = EpisodeReader::new(Arc::clone(&store));
    let err = reader
        .read_snapshot(&ctx, &task_id)
        .await
        .expect_err("poll-only task is not an episode");
    assert!(
        matches!(err, ProvenanceError::EpisodeUnbound { .. }),
        "expected EpisodeUnbound, got {err:?}"
    );
}

#[tokio::test]
async fn minted_tool_call_repoints_head_without_bootstrap_event() {
    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("store"),
    );
    let agent_id = test_agent_id();
    boot_agent(&store, agent_id.clone(), "dispatch-echo").await;

    let context_id = ContextId::new(1_778_546_320_939, 1);
    let task_id = TaskId::from_external(ExternalId::new(Uuid::new_v4().to_string()));
    let message_id = MessageId::from("system/callback:test-cb");

    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "system/discover_tools".to_string(),
            None,
            json!({"reason": "probe"}),
            json!({
                "phase": "send",
                "message_id": message_id.as_str(),
                "task_id": task_id.as_str(),
                "agent_id": agent_id.as_str(),
            }),
            None,
        ))
        .await
        .expect("tool_call_started");

    let head_rows = head_executed_by_rows(&store, &task_id).await;
    assert_eq!(
        head_rows.len(),
        1,
        "defense-in-depth must repoint head from metadata agent_id: {head_rows:?}"
    );

    let reader = EpisodeReader::new(Arc::clone(&store));
    let episode = reader
        .read_snapshot(&context_id, &task_id)
        .await
        .expect("episode");
    assert_eq!(episode.agent_id, agent_id);

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id),
                task_id: Some(task_id.clone()),
                tool_name: Some("system/discover_tools".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query_ops")
        .rows;
    assert!(!rows.is_empty(), "tool ops rows: {rows:?}");
}

#[tokio::test]
async fn episode_unbound_task_returns_error() {
    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("store"),
    );
    let ctx = ContextId::new(9, 9);
    let task_id = TaskId::from_external(ExternalId::new("unbound-task".to_string()));
    store
        .add_event(ProvEvent::task_exists(ctx.clone(), task_id.clone()))
        .await
        .expect("task_exists");

    let reader = EpisodeReader::new(Arc::clone(&store));
    let err = reader
        .read_snapshot(&ctx, &task_id)
        .await
        .expect_err("task_exists-only is not an episode");
    assert!(
        matches!(err, ProvenanceError::EpisodeUnbound { .. }),
        "expected EpisodeUnbound, got {err:?}"
    );
}
