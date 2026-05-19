use std::{fs, path::PathBuf, sync::Arc};

use baml_rt_core::{
    Outcome,
    bus::{BusWithEffects, EffectEmitter, ToolEffectMetadata},
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_provenance::{
    ProvenanceEffectSubscriber, ProvenanceOpsFilters, ProvenanceOpsQuery,
    ProvenanceOpsQueryRequest, ProvenanceOpsResource, SurrealStoreBuilder,
};
use serde_json::json;

fn agent() -> AgentId {
    AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()))
}

async fn assert_effect_bus_tool_rows(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) {
    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    bus.subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;

    let context_id = ContextId::new(1_778_675_600_000, 1);
    let task_id = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));
    let agent_id = agent();
    let metadata = ToolEffectMetadata {
        tool_name: "system/discover_tools".to_string(),
        function_name: None,
        args: json!({ "query": "callback-probe", "limit": 1 }),
        metadata: json!({
            "phase": "send",
            "message_id": "system/callback:test-callback",
            "task_id": task_id.as_str(),
            "agent_id": agent_id.as_str(),
        }),
        delegation_target: None,
        tool_backend: None,
        tool_digest: None,
    };

    let token = bus
        .start_tool(context_id.clone(), metadata)
        .await
        .expect("start tool");
    token
        .complete(
            bus.as_ref(),
            12,
            Outcome::Success,
            Some(json!({ "status": "sent" })),
        )
        .await
        .expect("complete tool");

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

    assert!(
        !rows.is_empty(),
        "effect-bus tool events should materialize in ops tool_calls rows"
    );
}

async fn assert_effect_bus_tool_rows_for_task_id(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    task_id: TaskId,
    context_id: ContextId,
) {
    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    bus.subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;

    let agent_id = agent();
    let metadata = ToolEffectMetadata {
        tool_name: "system/discover_tools".to_string(),
        function_name: None,
        args: json!({ "query": "callback-probe", "limit": 1 }),
        metadata: json!({
            "phase": "send",
            "message_id": "system/callback:test-callback",
            "task_id": task_id.as_str(),
            "agent_id": agent_id.as_str(),
        }),
        delegation_target: None,
        tool_backend: None,
        tool_digest: None,
    };

    let token = bus
        .start_tool(context_id.clone(), metadata)
        .await
        .expect("start tool");
    token
        .complete(
            bus.as_ref(),
            12,
            Outcome::Success,
            Some(json!({ "status": "sent" })),
        )
        .await
        .expect("complete tool");

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

    assert!(
        !rows.is_empty(),
        "effect-bus tool events should remain queryable for live-task style task ids"
    );

    let task_exec =
        baml_rt_provenance::id_semantics::task_execution_activity_id_string(task_id.as_str());
    let edges: Vec<serde_json::Value> = store
        .db()
        .query("SELECT from_id, to_id FROM prov_edge WHERE rel_type = 'A2A_TASK_CALL' AND from_id = $from")
        .bind(("from", task_exec))
        .await
        .expect("task_call edge query")
        .take(0)
        .expect("task_call edge rows");
    assert!(
        !edges.is_empty(),
        "effect-bus tool writes must emit A2A_TASK_CALL from TaskExecution; edges={edges:?}"
    );
}

#[tokio::test]
async fn effect_bus_tool_events_materialize_in_tool_ops_query_for_task_scope_in_memory() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory store");
    assert_effect_bus_tool_rows(store).await;
}

#[tokio::test]
async fn effect_bus_tool_events_materialize_in_tool_ops_query_for_task_scope_file_backed() {
    let path: PathBuf =
        std::env::temp_dir().join(format!("prov-effect-bus-tool-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temp provenance dir");
    let store = SurrealStoreBuilder::file(&path)
        .build()
        .await
        .expect("file-backed store");
    assert_effect_bus_tool_rows(store).await;
    let _ = fs::remove_dir_all(path);
}

#[tokio::test]
async fn effect_bus_tool_events_materialize_for_live_task_ids() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory store");
    let context_id = ContextId::new(730, 2);
    let task_id = TaskId::from_external(ExternalId::new(
        "live-task:ctx-730-2:dispatch-echo-resume-msg".to_string(),
    ));
    assert_effect_bus_tool_rows_for_task_id(store, task_id, context_id).await;
}
