// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Regression: ops tool-calls query must see host tool rows for a minted dispatch
//! context + UUID task without a prior `TaskExists` / `TaskExecutionStarted` bootstrap
//! (detached callback dispatch path).

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    ProvEvent,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
        ProvenanceWriter,
    },
};
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store;

fn test_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()))
}

#[tokio::test]
async fn tool_ops_query_finds_minted_dispatch_scope_without_task_bootstrap() {
    let store = build_isolated_store().await;

    let agent_id = test_agent_id();
    let context_id = ContextId::new(1_778_546_320_939, 1);
    let task_id = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));
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

    store
        .add_event(ProvEvent::tool_call_completed_task(
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
            10,
            baml_rt_core::Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
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
        "expected at least one tool_call row for minted scope; rows={rows:?}"
    );
}

/// `ProvEvent::Global` tool calls still carry `task_id` in effect metadata; graph writes must
/// emit `TASK_CALL` from `TaskExecution` using that metadata (not only `ProvEvent::Task`).
#[tokio::test]
async fn tool_ops_query_finds_global_envelope_when_metadata_carries_task_id() {
    let store = build_isolated_store().await;

    let agent_id = test_agent_id();
    let context_id = ContextId::new(1_778_546_320_940, 1);
    let task_id = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));
    let message_id = MessageId::from("system/callback:test-cb-global");

    store
        .add_event(ProvEvent::tool_call_started_global(
            context_id.clone(),
            message_id.clone(),
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
        .expect("tool_call_started_global");

    store
        .add_event(ProvEvent::tool_call_completed_global(
            context_id.clone(),
            message_id,
            "system/discover_tools".to_string(),
            None,
            json!({"reason": "probe"}),
            json!({
                "phase": "send",
                "message_id": "system/callback:test-cb-global",
                "task_id": task_id.as_str(),
                "agent_id": agent_id.as_str(),
            }),
            10,
            baml_rt_core::Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed_global");

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
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
        "expected tool_call rows for Global envelope + metadata task_id; rows={rows:?}"
    );
}
