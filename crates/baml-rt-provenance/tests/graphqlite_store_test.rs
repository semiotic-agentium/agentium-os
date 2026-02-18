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
