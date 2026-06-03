// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `query_conversation_context` with `agent_package` must not emit invalid Surreal `IN` syntax.

use baml_rt_core::{
    Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{ProvEvent, ProvenanceQueryApi, ProvenanceWriter, SurrealStoreBuilder};
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store;
use uuid::Uuid;

#[tokio::test]
async fn query_conversation_context_with_agent_package_succeeds() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(91, 92);
    store
        .add_event(baml_rt_provenance::ProvEvent::host_source_poll_recorded(
            ctx.clone(),
            "clickup".to_string(),
            "clickup:list:1".to_string(),
            "cursor:1".to_string(),
            "host.source-records.v1".to_string(),
            0,
            vec![],
        ))
        .await
        .expect("lineage only");

    let items = store
        .query_conversation_context(&ctx, Some(50), None, Some("clickup-agent"))
        .await
        .expect("agent_package filter must parse and execute");
    assert!(
        items.is_empty(),
        "no agent-scoped messages seeded; expected empty not error: {items:?}"
    );
}

#[tokio::test]
async fn query_conversation_context_with_agent_package_when_context_has_scoped_rows() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(93, 94);
    let task_id = TaskId::from_external(ExternalId::new(Uuid::new_v4().to_string()));
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));

    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_id.clone(),
            MessageId::from("scoped-msg-1"),
            "ROLE_USER".to_string(),
            vec!["hello".to_string()],
            None,
            agent_id,
            1_780_000_020_000,
        ))
        .await
        .expect("scoped message");

    let items = store
        .query_conversation_context(&ctx, Some(50), None, Some("clickup-agent"))
        .await
        .expect("agent_package filter must execute when SCOPED_TO rows exist");
    assert!(
        items.is_empty(),
        "no clickup archive; filter should return empty: {items:?}"
    );
}

#[tokio::test]
async fn query_conversation_context_with_agent_package_and_tool_call_row() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(95, 96);
    let task_id = TaskId::from_external(ExternalId::new(Uuid::new_v4().to_string()));
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));

    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_id.clone(),
            MessageId::from("msg-with-tool"),
            "ROLE_USER".to_string(),
            vec!["user line".to_string()],
            None,
            agent_id.clone(),
            1_780_000_030_000,
        ))
        .await
        .expect("message");
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            Some("list_tasks".to_string()),
            json!({}),
            json!({ "agent_id": agent_id.as_str() }),
            5,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool");

    store
        .query_conversation_context(&ctx, Some(50), None, Some("clickup-agent"))
        .await
        .expect("agent_package + ToolCall row must not error");
}

#[tokio::test]
async fn query_conversation_context_with_agent_package_on_file_backed_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SurrealStoreBuilder::file(dir.path())
        .build()
        .await
        .expect("file store");
    let ctx = ContextId::new(97, 98);
    let task_id = TaskId::from_external(ExternalId::new(Uuid::new_v4().to_string()));
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));

    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_id.clone(),
            MessageId::from("file-msg"),
            "ROLE_USER".to_string(),
            vec!["line".to_string()],
            None,
            agent_id.clone(),
            1_780_000_040_000,
        ))
        .await
        .expect("message");
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            task_id,
            "support/clickup".to_string(),
            None,
            json!({}),
            json!({ "agent_id": agent_id.as_str() }),
            1,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool");

    store
        .query_conversation_context(&ctx, Some(50), None, Some("clickup-agent"))
        .await
        .expect("file-backed store must run agent_package filter");
}
