// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt_core::{
    Outcome,
    bus::{BusWithEffects, EffectEmitter, ToolEffectMetadata},
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_provenance::{
    ProvenanceEffectSubscriber, ProvenanceOpsFilters, ProvenanceOpsQuery,
    ProvenanceOpsQueryRequest, ProvenanceOpsResource, SurrealStoreBuilder,
    episode::AgentGateActivityFilters,
};
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store;

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
    let store = build_isolated_store().await;
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
async fn effect_bus_gate_blocked_failure_materializes_tool_ops_row() {
    let store = build_isolated_store().await;
    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    bus.subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;

    let context_id = ContextId::new(1_778_676_100_000, 1);
    let task_id = TaskId::from_external(ExternalId::new(
        "live-task:ctx-1783676100000-1:msg-gate-test".to_string(),
    ));
    let agent_id = agent();
    let karakara_gate = json!({
        "toolName": "support/clickup",
        "tier": 3,
        "decision": "deny",
        "reasonCode": "requirements_not_met",
        "deficientNodes": ["confirm_delete"]
    });
    let metadata = ToolEffectMetadata {
        tool_name: "support/clickup".to_string(),
        function_name: None,
        args: json!({ "action": "DeleteTask", "task_id": "86abc123" }),
        metadata: json!({
            "phase": "open",
            "message_id": "system/callback:gate-block-test",
            "task_id": task_id.as_str(),
            "agent_id": agent_id.as_str(),
            "agent_package": "clickup-agent",
            "karakara_gate": karakara_gate,
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
        .complete(bus.as_ref(), 0, Outcome::Failure, None)
        .await
        .expect("complete tool");

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
                task_id: Some(task_id.clone()),
                tool_name: Some("support/clickup".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query_ops")
        .rows;

    assert!(
        !rows.is_empty(),
        "gate-blocked tool effect should appear in ops"
    );
    let row = rows[0].as_map();
    assert_eq!(
        row.get("activity_outcome").and_then(|v| v.as_str()),
        Some("Failed")
    );
    assert!(
        row.get("gate").or_else(|| row.get("a2a_gate")).is_some(),
        "gate-blocked tool row must carry a2a_gate for Trust activity reads"
    );

    let gate_rows = store
        .query_gate_activity(AgentGateActivityFilters {
            agent_package: None,
            from_timestamp_ms: 0,
            to_timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_millis() as u64
                + 86_400_000,
            incident_limit: 10,
            page_size: 100,
        })
        .await
        .expect("query_gate_activity")
        .rows;
    assert!(
        gate_rows.iter().any(|r| {
            r.as_map().get("tool_name").and_then(|v| v.as_str()) == Some("support/clickup")
        }),
        "gate activity fast path must index gate-blocked tool calls"
    );
}

#[tokio::test]
async fn effect_bus_tool_events_materialize_for_live_task_ids() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(730, 2);
    let task_id = TaskId::from_external(ExternalId::new(
        "live-task:ctx-730-2:dispatch-echo-resume-msg".to_string(),
    ));
    assert_effect_bus_tool_rows_for_task_id(store, task_id, context_id).await;
}

async fn emit_external_tool_call(
    bus: &Arc<dyn EffectEmitter>,
    context_id: &ContextId,
    task_id: &TaskId,
    agent_id: &AgentId,
    tool_digest: &str,
) {
    let metadata = ToolEffectMetadata {
        tool_name: "dev/meteo-tool".to_string(),
        function_name: None,
        args: json!({ "location_query": "Paris, France" }),
        metadata: json!({
            "phase": "read",
            "message_id": format!("msg-{}", task_id.as_str()),
            "task_id": task_id.as_str(),
            "agent_id": agent_id.as_str(),
        }),
        delegation_target: None,
        tool_backend: Some("External".to_string()),
        tool_digest: Some(tool_digest.to_string()),
    };
    let token = bus
        .start_tool(context_id.clone(), metadata)
        .await
        .expect("start tool");
    token
        .complete(
            bus.as_ref(),
            30,
            Outcome::Success,
            Some(json!({ "source": "open-meteo" })),
        )
        .await
        .expect("complete tool");
}

/// §16.2.5: every invocation records the exact tool bytes that served it, and
/// the digest is one content-addressed node shared across contexts.
#[tokio::test]
async fn external_tool_calls_record_the_serving_tool_digest() {
    let store = build_isolated_store().await;
    let bus: Arc<dyn EffectEmitter> = Arc::new(BusWithEffects::new());
    bus.subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;

    let digest = format!("sha256:{}", "ab".repeat(32));
    let agent_id = agent();
    let first_ctx = ContextId::new(1_778_675_700_000, 1);
    let second_ctx = ContextId::new(1_778_675_700_001, 1);
    let first_task = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));
    let second_task = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));
    emit_external_tool_call(&bus, &first_ctx, &first_task, &agent_id, &digest).await;
    emit_external_tool_call(&bus, &second_ctx, &second_task, &agent_id, &digest).await;

    let artifacts: Vec<serde_json::Value> = store
        .db()
        .query("SELECT node_id, props FROM prov_node WHERE label = 'ToolArtifact'")
        .await
        .expect("artifact query")
        .take(0)
        .expect("artifact rows");
    assert_eq!(artifacts.len(), 1, "one node per digest; got {artifacts:?}");
    assert_eq!(artifacts[0]["props"]["a2a_tool_digest"], json!(digest));
    let artifact_node = artifacts[0]["node_id"]
        .as_str()
        .expect("node_id")
        .to_string();

    let served: Vec<serde_json::Value> = store
        .db()
        .query(
            "SELECT from_id, from_label FROM prov_edge \
             WHERE rel_type = 'WAS_SERVED_BY' AND to_id = $to",
        )
        .bind(("to", artifact_node))
        .await
        .expect("served_by query")
        .take(0)
        .expect("served_by rows");
    assert!(
        served.len() >= 2 && served.iter().all(|e| e["from_label"] == json!("ToolCall")),
        "both contexts' ToolCalls must point at the artifact; got {served:?}"
    );

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                tool_digest: Some(digest.clone()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query_ops")
        .rows;
    assert!(!rows.is_empty(), "ops rows filterable by tool_digest");
    for row in &rows {
        let row = serde_json::to_value(row).expect("row json");
        assert_eq!(row["tool_digest"], json!(digest), "row={row}");
        assert_eq!(row["tool_backend"], json!("External"), "row={row}");
    }

    let none = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                tool_digest: Some(format!("sha256:{}", "cd".repeat(32))),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query_ops")
        .rows;
    assert!(none.is_empty(), "an unrelated digest matches nothing");
}
