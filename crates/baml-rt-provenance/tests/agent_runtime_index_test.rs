// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent runtime index registry behavior.

use baml_rt_core::{
    AgentInstanceId, AgentPackageName, AgentRouteKey, DispatchTarget, Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, LlmUsage, ProvEvent, ProvenanceWriter, SurrealStoreBuilder,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
    },
};
use uuid::Uuid;

#[tokio::test]
async fn agent_booted_upserts_package_instance_registry() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("perf_agent").expect("type"),
            "1.0.0".to_string(),
            "perf@1.0.0".to_string(),
        ))
        .await
        .expect("boot");

    let rows = store
        .db()
        .query("SELECT instance_node_id, agent_package, agent_id FROM agent_package_instance")
        .await
        .expect("query")
        .take::<Vec<serde_json::Value>>(0)
        .expect("rows");
    assert_eq!(rows.len(), 1, "expected one registry row: {rows:?}");
    assert_eq!(
        rows[0].get("agent_package").and_then(|v| v.as_str()),
        Some("perf_agent")
    );
    assert_eq!(
        rows[0].get("agent_id").and_then(|v| v.as_str()),
        Some(agent_id.as_str())
    );
}

#[tokio::test]
async fn dispatch_without_boot_does_not_index_route_stub() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let ctx = ContextId::new(1, 2);
    store
        .add_event(ProvEvent::host_dispatch_accepted(
            ctx,
            "event:intake".to_string(),
            "host.source-records.v1".to_string(),
            DispatchTarget::with_optional_agent(
                AgentRouteKey::new(
                    AgentPackageName::parse("orphan-agent").expect("package"),
                    AgentInstanceId::default(),
                ),
                None,
            ),
            "clickup".to_string(),
            "clickup:list:1".to_string(),
        ))
        .await
        .expect("dispatch without target agent");

    let rows = store
        .db()
        .query("SELECT instance_node_id, agent_package, agent_id FROM agent_package_instance")
        .await
        .expect("query")
        .take::<Vec<serde_json::Value>>(0)
        .expect("rows");
    assert!(
        rows.is_empty(),
        "non-boot dispatch must not upsert registry rows: {rows:?}"
    );
}

#[tokio::test]
async fn context_scoped_llm_ops_uses_registry_for_agent_package_filter() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    let context_id = ContextId::new(1_789_916_123_818, 1);
    let task_id = TaskId::from_external(ExternalId::new("dispatch-unit-test"));
    let message_id = MessageId::from("msg-1");

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("slack-agent").expect("type"),
            "1.0.0".to_string(),
            "slack-agent@1.0.0".to_string(),
        ))
        .await
        .expect("boot");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task execution");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            message_id.clone(),
            "user".to_string(),
            vec!["hello".to_string()],
            None,
            agent_id.clone(),
            1_789_916_123_818,
        ))
        .await
        .expect("message");
    store
        .add_event(ProvEvent::llm_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "DefaultClient".to_string(),
            "openai-generic".to_string(),
            "Chat".to_string(),
            serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}),
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
                "message_id": message_id.as_str(),
            }),
            LlmUsage::Known {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_input_tokens: None,
            },
            100,
            Outcome::Success,
        ))
        .await
        .expect("llm");

    let response = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::LlmCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id),
                task_id: Some(task_id),
                agent_package: Some("slack-agent".to_string()),
                ..Default::default()
            },
            group_by: vec![
                "agent_id".to_string(),
                "agent_package".to_string(),
                "agent_version".to_string(),
                "model".to_string(),
            ],
            ..Default::default()
        })
        .await
        .expect("context-scoped llm ops with agent package");

    assert_eq!(
        response.rows.len(),
        1,
        "registry-backed agent package filter must return seeded LLM row: {:?}",
        response.rows
    );
}
