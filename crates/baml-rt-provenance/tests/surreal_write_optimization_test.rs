// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for batched Surreal writes, payload blob offload, and hydration.

use std::sync::Arc;

use baml_rt_core::{
    Outcome,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceArchivePayload, ProvenanceError, ProvenanceOpsQuery,
    ProvenanceQueryApi, ProvenanceWriter, SurrealStoreBuilder,
};
use serde_json::{Value, json};

/// Must match [`baml_rt_provenance::surreal_tables::FTS_PAYLOAD_ACTIVITY_WHERE`] bind name `$query_text`.
const FTS_PAYLOAD_ACTIVITY_WHERE: &str = "search_text @@ $query_text AND activity_id IS NOT NONE";

async fn isolated_store() -> Arc<baml_rt_provenance::SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build store")
}

/// Boot → task → task execution → message → tool_call_started (shared prefix for blob/FTS tests).
async fn seed_through_tool_started(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
    agent_id: &AgentId,
    message_id: &MessageId,
) {
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("test_agent").expect("type"),
            "1.0.0".to_string(),
            "test@1.0.0".to_string(),
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
        .expect("te");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            message_id.clone(),
            "user".to_string(),
            vec!["ping".to_string()],
            None,
            agent_id.clone(),
            1,
        ))
        .await
        .expect("msg");
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "echo".to_string(),
            None,
            json!({}),
            json!({
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
                "message_id": message_id.as_str(),
                "phase": "started",
            }),
            None,
        ))
        .await
        .expect("tool start");
}

/// Tool results over `PAYLOAD_OFFLOAD_THRESHOLD_BYTES` are stored in `provenance_payload_blob`
/// and hydrated transparently on `resolve_archive_ref`.
#[tokio::test]
async fn large_tool_result_blob_roundtrip_via_resolve_archive_ref() {
    let store = isolated_store().await;
    let context_id = ContextId::new(42, 42);
    let task_id = TaskId::from_external(ExternalId::new("task-blob-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
    let message_id = MessageId::from_external(ExternalId::new("msg-blob-1"));

    // Exceeds `payload_storage::PAYLOAD_OFFLOAD_THRESHOLD_BYTES` (16 KiB) to force blob offload.
    let huge = "xy".repeat(12_000);
    assert!(huge.len() > 16 * 1024);

    seed_through_tool_started(
        store.as_ref(),
        &context_id,
        &task_id,
        &agent_id,
        &message_id,
    )
    .await;

    let completed = ProvEvent::tool_call_completed_task(
        context_id,
        task_id,
        "echo".to_string(),
        None,
        json!({}),
        json!({
            "agent_id": agent_id.as_str(),
            "message_id": message_id.as_str(),
            "phase": "complete",
            "result": { "blob": huge.clone() },
        }),
        1,
        Outcome::Success,
        None,
    );
    let anchor = completed.id().as_str().to_string();
    store.add_event(completed).await.expect("tool done");

    let payload_id = format!("payload:{anchor}:tool_result");
    let archive_ref = format!("prov:v1:payload:{payload_id}");
    let resolved = store
        .resolve_archive_ref(&archive_ref)
        .await
        .expect("resolve")
        .expect("some record");
    let payloads = resolved.payloads;
    assert_eq!(payloads.len(), 1);
    match &payloads[0] {
        ProvenanceArchivePayload::ToolResult { result_json, .. } => {
            let v: serde_json::Value = serde_json::from_str(result_json.as_str()).expect("json");
            assert_eq!(v.get("blob").and_then(|x| x.as_str()), Some(huge.as_str()));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

/// `search_text` on blob-offloaded rows is still BM25-indexed; `@@` must find the activity.
#[tokio::test]
async fn blob_payload_search_text_is_fulltext_indexed() {
    let store = isolated_store().await;
    let context_id = ContextId::new(43, 43);
    let task_id = TaskId::from_external(ExternalId::new("task-fts-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000088").unwrap());
    let message_id = MessageId::from_external(ExternalId::new("msg-fts-1"));
    let fts_token = "splorp9xyztokenfts_unique";

    let huge = "zz".repeat(12_000);
    assert!(huge.len() > 16 * 1024);

    seed_through_tool_started(
        store.as_ref(),
        &context_id,
        &task_id,
        &agent_id,
        &message_id,
    )
    .await;

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id,
            task_id,
            "echo".to_string(),
            None,
            json!({}),
            json!({
                "agent_id": agent_id.as_str(),
                "message_id": message_id.as_str(),
                "phase": "complete",
                "result": json!({
                    "aaa_fts_marker": fts_token,
                    "blob": huge,
                }),
            }),
            1,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool done");

    let mut response = store
        .db()
        .query(format!(
            "SELECT activity_id FROM provenance_payload WHERE {FTS_PAYLOAD_ACTIVITY_WHERE}"
        ))
        .bind(("query_text", fts_token.to_string()))
        .await
        .expect("fts query");
    let rows: Vec<Value> = response.take(0).expect("take rows");
    let ids: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("activity_id").and_then(Value::as_str))
        .collect();
    assert!(
        ids.iter().any(|id| !id.is_empty()),
        "expected at least one activity_id from FTS, got {ids:?}"
    );
}

/// Invalid statement inside `BEGIN…COMMIT` must not leave prior statements committed.
#[tokio::test]
async fn transaction_aborts_on_invalid_statement() {
    let store = isolated_store().await;
    let bad_batch = "BEGIN;\n\
        UPSERT prov_node SET node_id = 'txn_abort_probe', label = 'ProvEntity' WHERE node_id = 'txn_abort_probe';\n\
        ¢¢NOT_VALID_SURQL¢¢;\n\
        COMMIT;";
    let err = store.db().query(bad_batch).await;
    assert!(
        err.is_err(),
        "expected Surreal parse/execute error, got {err:?}"
    );

    let mut response = store
        .db()
        .query("SELECT node_id FROM prov_node WHERE node_id = 'txn_abort_probe' LIMIT 1")
        .await
        .expect("probe");
    let rows: Vec<Value> = response.take(0).expect("take");
    assert!(
        rows.is_empty(),
        "node should not persist if txn aborted, got {rows:?}"
    );
}

#[tokio::test]
async fn corrupt_payload_row_errors_on_resolve() {
    let store = isolated_store().await;
    let context_id = ContextId::new(44, 44);
    let task_id = TaskId::from_external(ExternalId::new("task-corrupt-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000077").unwrap());
    let message_id = MessageId::from_external(ExternalId::new("msg-corrupt-1"));

    seed_through_tool_started(
        store.as_ref(),
        &context_id,
        &task_id,
        &agent_id,
        &message_id,
    )
    .await;

    let completed = ProvEvent::tool_call_completed_task(
        context_id,
        task_id,
        "echo".to_string(),
        None,
        json!({}),
        json!({
            "agent_id": agent_id.as_str(),
            "message_id": message_id.as_str(),
            "phase": "complete",
            "result": json!({ "ok": true }),
        }),
        1,
        Outcome::Success,
        None,
    );
    let anchor = completed.id().as_str().to_string();
    store.add_event(completed).await.expect("tool done");

    let payload_id = format!("payload:{anchor}:tool_result");
    store
        .db()
        .query("UPDATE provenance_payload SET storage_kind = 42 WHERE payload_id = $pid")
        .bind(("pid", payload_id.clone()))
        .await
        .expect("poison row");

    let archive_ref = format!("prov:v1:payload:{payload_id}");
    let err = store
        .resolve_archive_ref(&archive_ref)
        .await
        .expect_err("expected corrupt row error");
    assert!(
        matches!(err, ProvenanceError::CorruptPayloadRow { .. }),
        "expected CorruptPayloadRow, got {err:?}"
    );
}

/// Blob offload rows hydrate via a single `content_hash IN (...)` fetch per batch.
#[tokio::test]
async fn batch_hydrates_multiple_blob_payloads_in_one_fetch() {
    let store = isolated_store().await;
    let context_id = ContextId::new(55, 55);
    let task_id = TaskId::from_external(ExternalId::new("task-batch-hydrate"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000088").unwrap());
    let message_id = MessageId::from_external(ExternalId::new("msg-batch-hydrate"));

    seed_through_tool_started(
        store.as_ref(),
        &context_id,
        &task_id,
        &agent_id,
        &message_id,
    )
    .await;

    let huge = "x".repeat(20 * 1024);
    for phase in ["started", "complete"] {
        let event = if phase == "started" {
            ProvEvent::tool_call_started_task(
                context_id.clone(),
                task_id.clone(),
                "echo".to_string(),
                None,
                json!({}),
                json!({
                    "agent_id": agent_id.as_str(),
                    "message_id": message_id.as_str(),
                    "phase": phase,
                }),
                None,
            )
        } else {
            ProvEvent::tool_call_completed_task(
                context_id.clone(),
                task_id.clone(),
                "echo".to_string(),
                None,
                json!({}),
                json!({
                    "agent_id": agent_id.as_str(),
                    "message_id": message_id.as_str(),
                    "phase": phase,
                    "result": json!({ "blob": huge }),
                }),
                2,
                Outcome::Success,
                None,
            )
        };
        store.add_event(event).await.expect("tool event");
    }

    let convo = store
        .query_conversation_context(&context_id, None, Some(&task_id), None)
        .await
        .expect("conversation context");
    assert!(
        !convo.is_empty(),
        "expected hydrated conversation rows after blob batch fetch"
    );
}
