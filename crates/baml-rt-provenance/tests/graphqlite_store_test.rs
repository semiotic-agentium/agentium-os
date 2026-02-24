//! Integration tests for the GraphQLite provenance store.
//! Uses in-memory builder; no testcontainers.
//!
//! ## No-stale-read invariant
//!
//! Tests [graphqlite_store_no_stale_read_after_interleaved_writes] and
//! [graphqlite_store_persists_messages_and_returns_conversation_context] verify that
//! [ProvenanceContextReader::context_messages] and [ProvenanceContextReader::conversation_context]
//! reflect all prior completed writes (no stale read). Other provenance queries do not require
//! this guarantee.
//!
//! ## Test coverage (task and conversation context)
//!
//! | Test | What it checks |
//! |------|-----------------|
//! | `graphqlite_store_persists_messages_and_returns_conversation_context` | Batch write (4 events) then read; 2 messages and 2 conversation items. Implicitly no stale read after batch. |
//! | `graphqlite_store_no_stale_read_after_interleaved_writes` | Write → read (1) → write → read (2). Explicit no-stale-read: second read must see both messages. |
//!
//! These tests run safely with default test concurrency: Cypher execution is serialized
//! process-wide in the GraphQLite store (see module docs in `graphqlite_store`).
//!
//! ## Agent vs API read types
//!
//! [ProvenanceContextReader] (no-stale-read) is used by the agent runtime; [ProvenanceQueryApi]
//! (no guarantee) is for API-exposed reads. The same store implements both; the type system
//! enforces which guarantee the caller gets. API handlers should take `Arc<dyn ProvenanceQueryApi>`.

use std::collections::HashSet;

use baml_rt_core::ids::{AgentId, ContextId, EventId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, GlobalEvent, GraphqliteStoreBuilder, ProvEvent, ProvEventData,
    ProvenanceContextReader, ProvenanceQueryApi, ProvenanceWriter, TaskScopedEvent,
};
use tempfile::tempdir;

#[tokio::test]
async fn graphqlite_store_persists_messages_and_returns_conversation_context() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance.db");
    let store = GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    let events = [
        ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(0),
            context_id: context_id.clone(),
            timestamp_ms: 1_700_000_000_000,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }),
        ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_001,
            data: ProvEventData::TaskCreated {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
            },
        }),
        ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(2),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_002,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-1")),
                role: "user".to_string(),
                content: vec!["Hello".to_string()],
                metadata: None,
            },
        }),
        ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(3),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_003,
            data: ProvEventData::MessageSent {
                id: MessageId::from_external(ExternalId::new("msg-2")),
                role: "assistant".to_string(),
                content: vec!["Hi there.".to_string()],
                metadata: None,
            },
        }),
    ];

    for event in &events {
        store.add_event(event.clone()).await.expect("add_event");
    }

    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("context_messages");
    assert_eq!(messages.len(), 2, "expect user + assistant message");
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content, vec!["Hello"]);
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].content, vec!["Hi there."]);

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].role, "user");
    assert_eq!(items[0].source, "message");
    assert_eq!(items[1].role, "assistant");
    assert_eq!(items[1].source, "message");
}

/// No-stale-read invariant: a read after each write must see that write.
/// Write one message → read context_messages (expect 1) → write second message → read (expect 2).
/// Same for conversation_context. Would fail if the store returned cached/stale data on the second read.
#[tokio::test]
async fn graphqlite_store_no_stale_read_after_interleaved_writes() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance.db");
    let store = GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    // Bootstrap: AgentBooted + TaskCreated (no messages yet).
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(0),
            context_id: context_id.clone(),
            timestamp_ms: 1_700_000_000_000,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("add_event");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_001,
            data: ProvEventData::TaskCreated {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("add_event");

    // First message (user).
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(2),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_002,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-1")),
                role: "user".to_string(),
                content: vec!["First".to_string()],
                metadata: None,
            },
        }))
        .await
        .expect("add_event");

    // Read immediately after first message: must see 1 message (no stale read).
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("context_messages");
    assert_eq!(
        messages.len(),
        1,
        "no-stale-read: first read must see first message"
    );
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content, vec!["First"]);

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        1,
        "no-stale-read: first read must see first message in conversation_context"
    );
    assert_eq!(items[0].role, "user");
    assert_eq!(items[0].source, "message");

    // Second message (assistant).
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(3),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_003,
            data: ProvEventData::MessageSent {
                id: MessageId::from_external(ExternalId::new("msg-2")),
                role: "assistant".to_string(),
                content: vec!["Second".to_string()],
                metadata: None,
            },
        }))
        .await
        .expect("add_event");

    // Read immediately after second message: must see 2 messages (no stale read).
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("context_messages");
    assert_eq!(
        messages.len(),
        2,
        "no-stale-read: second read must see both messages"
    );
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content, vec!["First"]);
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].content, vec!["Second"]);

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    assert_eq!(
        items.len(),
        2,
        "no-stale-read: second read must see both messages in conversation_context"
    );
    assert_eq!(items[0].role, "user");
    assert_eq!(items[1].role, "assistant");
}

/// API path uses [ProvenanceQueryApi]; no guarantee of no-stale-read. Same data as agent path
/// when using the same store; the type enforces that API callers use this trait.
#[tokio::test]
async fn graphqlite_store_query_api_returns_same_shape_as_reader() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance.db");
    let store = GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(0),
            context_id: context_id.clone(),
            timestamp_ms: 1_700_000_000_000,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("add_event");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_001,
            data: ProvEventData::TaskCreated {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("add_event");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(2),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_002,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-1")),
                role: "user".to_string(),
                content: vec!["Hello".to_string()],
                metadata: None,
            },
        }))
        .await
        .expect("add_event");

    let messages = store
        .query_context_messages(&context_id, None)
        .await
        .expect("query_context_messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");

    let items = store
        .query_conversation_context(&context_id, None)
        .await
        .expect("query_conversation_context");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, "message");
}

/// Reproduce the ClickUp agent bug: tool calls written to graphqlite must
/// appear in conversation_context as tool_call + tool_result items.
#[tokio::test]
async fn graphqlite_conversation_context_includes_tool_calls() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance_tool.db");
    let store = GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(42, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-tool-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

    // 1) AgentBooted + TaskCreated (required bootstrap)
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(100),
            context_id: context_id.clone(),
            timestamp_ms: 1_700_000_000_000,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("AgentBooted");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(101),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_001,
            data: ProvEventData::TaskCreated {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("TaskCreated");

    // 2) User message
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(102),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_002,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-1")),
                role: "user".to_string(),
                content: vec!["list my tasks".to_string()],
                metadata: None,
            },
        }))
        .await
        .expect("MessageReceived");

    // 3) ToolCallStarted (send phase) — mirrors what the interceptor writes
    let tool_args = serde_json::json!({"action":"ListTeams"});
    let started_metadata = serde_json::json!({
        "message_id": "msg-1",
        "task_id": "task-tool-1",
        "agent_id": "00000000-0000-0000-0000-000000000099",
        "phase": "send"
    });
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            None,
            tool_args.clone(),
            started_metadata,
        ))
        .await
        .expect("ToolCallStarted");

    // 4) ToolCallCompleted (send phase with result) — the key event
    let completed_metadata = serde_json::json!({
        "message_id": "msg-1",
        "task_id": "task-tool-1",
        "agent_id": "00000000-0000-0000-0000-000000000099",
        "phase": "send",
        "result": {
            "tasks": [],
            "items": [{"id": "9013491519", "name": "Test Workspace", "kind": "team"}],
            "message": "Found 1 team(s)"
        }
    });
    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            None,
            tool_args,
            completed_metadata,
            616,
            baml_rt_core::Outcome::Success,
        ))
        .await
        .expect("ToolCallCompleted");

    // 5) Read conversation_context — must include exactly one tool_call + tool_result
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    let tool_call_items: Vec<_> = items.iter().filter(|i| i.source == "tool_call").collect();
    let tool_result_items: Vec<_> = items.iter().filter(|i| i.source == "tool_result").collect();
    assert_eq!(
        tool_call_items.len(),
        1,
        "expected exactly one tool_call item, got {} items total: {:?}",
        items.len(),
        items.iter().map(|i| &i.source).collect::<Vec<_>>()
    );
    assert!(
        tool_result_items.len() == 1,
        "expected exactly one tool_result item, got {} items total: {:?}",
        items.len(),
        items.iter().map(|i| &i.source).collect::<Vec<_>>()
    );
    let unique_tool_items: HashSet<String> = items
        .iter()
        .filter(|i| i.source.starts_with("tool_"))
        .map(|i| format!("{}|{}|{}", i.event_id, i.source, i.content))
        .collect();
    assert_eq!(
        unique_tool_items.len(),
        tool_call_items.len() + tool_result_items.len(),
        "duplicate tool_call/tool_result entries detected: {:?}",
        items
            .iter()
            .filter(|i| i.source.starts_with("tool_"))
            .map(|i| format!("{}|{}|{}", i.event_id, i.source, i.content))
            .collect::<Vec<_>>()
    );
    let result_message = tool_result_items[0]
        .content
        .get("result")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str());
    assert_eq!(result_message, Some("Found 1 team(s)"));
}

/// Failed tool calls must still appear in conversation_context as tool_result
/// entries so retries/recovery can use prior failure context.
#[tokio::test]
async fn graphqlite_conversation_context_includes_failed_tool_results() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance_tool_failed.db");
    let store = GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(43, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-tool-failure"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000098").unwrap());

    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(200),
            context_id: context_id.clone(),
            timestamp_ms: 1_700_000_000_100,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("AgentBooted");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(201),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_101,
            data: ProvEventData::TaskCreated {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("TaskCreated");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(202),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_102,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-failure")),
                role: "user".to_string(),
                content: vec!["list all tasks".to_string()],
                metadata: None,
            },
        }))
        .await
        .expect("MessageReceived");

    let tool_args = serde_json::json!({"tasks_action":"ListTasks"});
    let started_metadata = serde_json::json!({
        "message_id": "msg-failure",
        "task_id": "task-tool-failure",
        "agent_id": "00000000-0000-0000-0000-000000000098",
        "phase": "send"
    });
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            None,
            tool_args.clone(),
            started_metadata,
        ))
        .await
        .expect("ToolCallStarted");

    let completed_metadata = serde_json::json!({
        "message_id": "msg-failure",
        "task_id": "task-tool-failure",
        "agent_id": "00000000-0000-0000-0000-000000000098",
        "phase": "send",
        "error": "Invalid argument: list_id is required"
    });
    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id,
            "support/clickup".to_string(),
            None,
            tool_args,
            completed_metadata,
            23,
            baml_rt_core::Outcome::Failure,
        ))
        .await
        .expect("ToolCallCompletedFailure");

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    let tool_call_items: Vec<_> = items.iter().filter(|i| i.source == "tool_call").collect();
    let tool_result_items: Vec<_> = items.iter().filter(|i| i.source == "tool_result").collect();
    assert_eq!(
        tool_call_items.len(),
        1,
        "failed call should still emit tool_call"
    );
    assert_eq!(
        tool_result_items.len(),
        1,
        "failed call should still emit tool_result"
    );
    let unique_tool_items: HashSet<String> = items
        .iter()
        .filter(|i| i.source.starts_with("tool_"))
        .map(|i| format!("{}|{}|{}", i.event_id, i.source, i.content))
        .collect();
    assert_eq!(
        unique_tool_items.len(),
        tool_call_items.len() + tool_result_items.len(),
        "duplicate tool entries detected"
    );
    let error_message = tool_result_items[0]
        .content
        .get("error")
        .and_then(|v| v.as_str());
    assert_eq!(
        error_message,
        Some("Invalid argument: list_id is required"),
        "failed result should carry metadata.error"
    );
}

#[tokio::test]
async fn graphqlite_tool_call_writes_enforce_args_edge_role_and_type() {
    let dir = tempdir().expect("tempdir");
    let path = dir.keep().join("provenance_tool_contract.db");
    let store = GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build store");

    let context_id = ContextId::new(77, 9);
    let task_id = TaskId::from_external(ExternalId::new("task-contract-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000097").unwrap());

    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(300),
            context_id: context_id.clone(),
            timestamp_ms: 1_700_000_100_000,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("AgentBooted");
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(301),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_100_001,
            data: ProvEventData::TaskCreated {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("TaskCreated");

    let args = serde_json::json!({ "task_id": "task-901" });
    let metadata = serde_json::json!({
        "message_id": "msg-contract-1",
        "task_id": "task-contract-1",
        "agent_id": "00000000-0000-0000-0000-000000000097",
        "phase": "send",
        "result": { "tasks": [] }
    });
    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id,
            "support/clickup".to_string(),
            None,
            args,
            metadata,
            12,
            baml_rt_core::Outcome::Success,
        ))
        .await
        .expect("ToolCallCompleted");

    let mut params = serde_json::Map::new();
    params.insert(
        "context".to_string(),
        serde_json::Value::String(context_id.as_str().to_string()),
    );
    let rows = store
        .run_cypher_read(
            "MATCH (t:ToolCall)-[used:WAS_USED_BY]->(args:ToolArgs) \
             WHERE t.a2a_context_id = $context \
             RETURN toString(properties(used)) AS rel_props, used.prov_base_type AS rel_base_type, \
                    used.prov_role AS role, args.prov_type AS target_type \
             ORDER BY t.a2a_event_id LIMIT 1",
            &params,
        )
        .await
        .expect("query args-edge contract row");
    let row = rows
        .iter()
        .next()
        .expect("at least one ToolCall->ToolArgs edge");
    let edge_role: Option<String> = row.get("role").ok();
    let rel_props: Option<String> = row.get("rel_props").ok();
    let rel_base_type: Option<String> = row.get("rel_base_type").ok();
    assert!(
        edge_role.as_deref() == Some("a2a:args") || edge_role.as_deref() == Some(""),
        "unexpected role value for ToolCall->ToolArgs edge: {edge_role:?}; rel_base_type={rel_base_type:?} rel_props={rel_props:?}"
    );
    let node_type: Option<String> = row.get("target_type").ok();
    assert_eq!(
        node_type.as_deref(),
        Some("a2a:ToolArgs"),
        "ToolArgs node must carry prov:type=a2a:ToolArgs"
    );
}
