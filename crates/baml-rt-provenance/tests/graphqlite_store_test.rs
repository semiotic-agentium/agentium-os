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

use baml_rt_core::{
    bus::PlanningSupersessionKind,
    ids::{AgentId, ContextId, EventId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentBootedEvent, AgentType, GraphqliteStoreBuilder, ProvEvent, ProvEventData,
    ProvenanceContextReader, ProvenancePlanningQuery, ProvenanceQueryApi, ProvenanceWriter,
    TaskScopedEvent,
};
use insta::assert_json_snapshot;

#[tokio::test]
async fn graphqlite_store_persists_messages_and_returns_conversation_context() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    let events = [
        ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(0),
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
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }),
        ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(2),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_002,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }),
        ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(3),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_003,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-1")),
                role: "user".to_string(),
                content: vec!["Hello".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }),
        ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(4),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_004,
            data: ProvEventData::MessageSent {
                id: MessageId::from_external(ExternalId::new("msg-2")),
                role: "assistant".to_string(),
                content: vec!["Hi there.".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
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
    assert_eq!(messages[0].role, "ROLE_USER");
    assert_eq!(messages[0].content, vec!["Hello"]);
    assert_eq!(messages[1].role, "ROLE_AGENT");
    assert_eq!(messages[1].content, vec!["Hi there."]);

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].role, "ROLE_USER");
    assert_eq!(items[0].source, "message");
    assert_eq!(items[1].role, "ROLE_AGENT");
    assert_eq!(items[1].source, "message");
}

/// No-stale-read invariant: a read after each write must see that write.
/// Write one message → read context_messages (expect 1) → write second message → read (expect 2).
/// Same for conversation_context. Would fail if the store returned cached/stale data on the second read.
#[tokio::test]
async fn graphqlite_store_no_stale_read_after_interleaved_writes() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    // Bootstrap: AgentBooted + TaskExists + TaskExecutionStarted (no messages yet).
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(0),
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
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("add_event");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
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
                agent_id: agent_id.clone(),
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
    assert_eq!(messages[0].role, "ROLE_USER");
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
    assert_eq!(items[0].role, "ROLE_USER");
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
                agent_id: agent_id.clone(),
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
    assert_eq!(messages[0].role, "ROLE_USER");
    assert_eq!(messages[0].content, vec!["First"]);
    assert_eq!(messages[1].role, "ROLE_AGENT");
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
    assert_eq!(items[0].role, "ROLE_USER");
    assert_eq!(items[1].role, "ROLE_AGENT");
}

/// API path uses [ProvenanceQueryApi]; no guarantee of no-stale-read. Same data as agent path
/// when using the same store; the type enforces that API callers use this trait.
#[tokio::test]
async fn graphqlite_store_query_api_returns_same_shape_as_reader() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(1, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(0),
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
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("add_event");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
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
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("add_event");

    let messages = store
        .query_context_messages(&context_id, None)
        .await
        .expect("query_context_messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "ROLE_USER");

    let items = store
        .query_conversation_context(&context_id, None)
        .await
        .expect("query_conversation_context");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, "message");
}

#[tokio::test]
async fn graphqlite_planning_query_returns_current_and_history() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(88, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-planning-query-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000095").unwrap());
    let msg_1 = MessageId::from_external(ExternalId::new("msg-planning-query-1"));
    let msg_2 = MessageId::from_external(ExternalId::new("msg-planning-query-2"));

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(400),
            timestamp_ms: 1_700_000_000_300,
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
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            msg_1.clone(),
            "user".to_string(),
            vec!["plan v1".to_string()],
            None,
            agent_id.clone(),
            1_700_000_000_301,
        ))
        .await
        .expect("message_received_1");
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "First intent".to_string(),
            vec![msg_1.clone()],
            None,
        ))
        .await
        .expect("intent v1");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "plan-v1".to_string(),
            vec![baml_rt_provenance::PlanStepSpec {
                step_id: baml_rt_core::ids::PlanStepId::from("step-v1"),
                description: "do first thing".to_string(),
                order: 0,
                depends_on: vec![],
            }],
            None,
        ))
        .await
        .expect("plan v1");

    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            msg_2.clone(),
            "user".to_string(),
            vec!["plan v2".to_string()],
            None,
            agent_id.clone(),
            1_700_000_000_302,
        ))
        .await
        .expect("message_received_2");
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "Second intent".to_string(),
            vec![msg_2],
            None,
        ))
        .await
        .expect("intent v2");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "plan-v2".to_string(),
            vec![baml_rt_provenance::PlanStepSpec {
                step_id: baml_rt_core::ids::PlanStepId::from("step-v2"),
                description: "do second thing".to_string(),
                order: 0,
                depends_on: vec![],
            }],
            None,
        ))
        .await
        .expect("plan v2");

    let replaced_rows = store
        .run_cypher_read(
            "MATCH (a)-[r]->(b) \
             WHERE a.a2a_task_id = $task_id AND b.a2a_task_id = $task_id \
               AND ((a:Intent AND b:Intent) OR (a:Plan AND b:Plan)) \
             RETURN count(r) AS replaced_count",
            &serde_json::json!({ "task_id": task_id.as_str() })
                .as_object()
                .cloned()
                .expect("params object"),
        )
        .await
        .expect("query replaced-by edges");
    let replaced_count = replaced_rows
        .iter()
        .next()
        .and_then(|row| row.get::<i64>("replaced_count").ok())
        .unwrap_or_default();
    assert!(
        replaced_count >= 2,
        "expected replacement edges for intent+plan revisions, got {replaced_count}"
    );

    let current_intent = store
        .query_current_intent(&task_id)
        .await
        .expect("query current intent")
        .expect("current intent exists");
    assert_eq!(current_intent.intent_id, "intent-v2");

    let current_plan = store
        .query_current_plan(&task_id)
        .await
        .expect("query current plan")
        .expect("current plan exists");
    assert_eq!(current_plan.plan_id, "plan-v2");
    assert_eq!(current_plan.intent_id, "intent-v2");
    assert_eq!(current_plan.steps.len(), 1);
    assert_eq!(current_plan.steps[0].step_id, "step-v2");
    assert_eq!(current_plan.steps[0].status, "ready");

    let intent_history = store
        .query_intent_history(&task_id, Some(10))
        .await
        .expect("query intent history");
    assert_eq!(intent_history.len(), 2);
    assert_eq!(intent_history[0].intent_id, "intent-v2");
    assert_eq!(intent_history[1].intent_id, "intent-v1");
    assert_eq!(
        intent_history[0].supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(intent_history[0].superseded_by_next, None);
    assert_eq!(intent_history[1].supersession_from_previous, None);
    assert_eq!(
        intent_history[1].superseded_by_next,
        Some(PlanningSupersessionKind::ReplacedBy)
    );

    let plan_history = store
        .query_plan_history(&task_id, Some(10))
        .await
        .expect("query plan history");
    assert_eq!(plan_history.len(), 2);
    assert_eq!(plan_history[0].plan_id, "plan-v2");
    assert_eq!(plan_history[1].plan_id, "plan-v1");
    assert_eq!(
        plan_history[0].supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(plan_history[0].superseded_by_next, None);
    assert_eq!(plan_history[1].supersession_from_previous, None);
    assert_eq!(
        plan_history[1].superseded_by_next,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
}

#[tokio::test]
async fn graphqlite_planning_current_selection_prefers_replacement_sink_over_order() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(89, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-planning-sink-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000094").unwrap());
    let msg = MessageId::from_external(ExternalId::new("msg-planning-sink-1"));

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(500),
            timestamp_ms: 1_700_000_000_400,
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
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            msg.clone(),
            "user".to_string(),
            vec!["plan sink test".to_string()],
            None,
            agent_id.clone(),
            1_700_000_000_401,
        ))
        .await
        .expect("message_received");

    // Create v1 -> v2 replacements.
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "old intent".to_string(),
            vec![msg.clone()],
            None,
        ))
        .await
        .expect("intent v1");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "plan-v1".to_string(),
            vec![baml_rt_provenance::PlanStepSpec {
                step_id: baml_rt_core::ids::PlanStepId::from("step-v1"),
                description: "v1".to_string(),
                order: 0,
                depends_on: vec![],
            }],
            None,
        ))
        .await
        .expect("plan v1");
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "new intent".to_string(),
            vec![msg.clone()],
            None,
        ))
        .await
        .expect("intent v2");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "plan-v2".to_string(),
            vec![baml_rt_provenance::PlanStepSpec {
                step_id: baml_rt_core::ids::PlanStepId::from("step-v2"),
                description: "v2".to_string(),
                order: 0,
                depends_on: vec![],
            }],
            None,
        ))
        .await
        .expect("plan v2");

    // Introduce a newer unrelated intent/plan on another task to ensure task scoping is strict.
    let other_task = TaskId::from_external(ExternalId::new("task-planning-sink-2"));
    store
        .add_event(ProvEvent::task_exists(
            context_id.clone(),
            other_task.clone(),
        ))
        .await
        .expect("other task exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            other_task.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("other task execution started");
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            other_task.clone(),
            "intent-other".to_string(),
            "other".to_string(),
            vec![msg.clone()],
            None,
        ))
        .await
        .expect("other intent");
    store
        .add_event(ProvEvent::plan_generated(
            context_id,
            other_task,
            "intent-other".to_string(),
            "plan-other".to_string(),
            vec![],
            None,
        ))
        .await
        .expect("other plan");

    let current_intent = store
        .query_current_intent(&task_id)
        .await
        .expect("query current intent")
        .expect("current intent exists");
    let current_plan = store
        .query_current_plan(&task_id)
        .await
        .expect("query current plan")
        .expect("current plan exists");

    assert_eq!(current_intent.intent_id, "intent-v2");
    assert_eq!(current_plan.plan_id, "plan-v2");
    assert_eq!(current_plan.intent_id, "intent-v2");
    assert_eq!(
        current_intent.supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(
        current_plan.supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
}

#[tokio::test]
async fn graphqlite_planning_current_selection_treats_refined_edges_as_superseding() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(90, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-planning-refined-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000095").unwrap());
    let msg = MessageId::from_external(ExternalId::new("msg-planning-refined-1"));

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("test").expect("agent type"),
            "1.0.0".to_string(),
            "test@1.0.0".to_string(),
        ))
        .await
        .expect("agent boot");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task execution started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            msg.clone(),
            "user".to_string(),
            vec!["refine test".to_string()],
            None,
            agent_id,
            1_700_000_000_501,
        ))
        .await
        .expect("message received");

    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "old intent".to_string(),
            vec![msg.clone()],
            None,
        ))
        .await
        .expect("intent v1");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "plan-v1".to_string(),
            vec![],
            None,
        ))
        .await
        .expect("plan v1");

    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "refined intent".to_string(),
            vec![msg.clone()],
            Some(PlanningSupersessionKind::RefinedBy),
        ))
        .await
        .expect("intent v2");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "plan-v2".to_string(),
            vec![],
            Some(PlanningSupersessionKind::RefinedBy),
        ))
        .await
        .expect("plan v2");

    let current_intent = store
        .query_current_intent(&task_id)
        .await
        .expect("query current intent")
        .expect("current intent exists");
    let current_plan = store
        .query_current_plan(&task_id)
        .await
        .expect("query current plan")
        .expect("current plan exists");
    assert_eq!(current_intent.intent_id, "intent-v2");
    assert_eq!(current_plan.plan_id, "plan-v2");

    let refined_rows = store
        .run_cypher_read(
            "MATCH (a)-[r:WAS_REFINED_BY]->(b) \
             WHERE a.a2a_task_id = $task_id AND b.a2a_task_id = $task_id \
               AND ((a:Intent AND b:Intent) OR (a:Plan AND b:Plan)) \
             RETURN count(r) AS refined_count",
            &serde_json::json!({ "task_id": task_id.as_str() })
                .as_object()
                .cloned()
                .expect("params object"),
        )
        .await
        .expect("query refined-by edges");
    let refined_count = refined_rows
        .iter()
        .next()
        .and_then(|row| row.get::<i64>("refined_count").ok())
        .unwrap_or_default();
    assert!(
        refined_count >= 2,
        "expected refined edges for intent+plan revisions, got {refined_count}"
    );
}

#[tokio::test]
async fn graphqlite_planning_history_tracks_mixed_supersession_chain_consistently() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(91, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-planning-mixed-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000096").unwrap());
    let msg = MessageId::from_external(ExternalId::new("msg-planning-mixed-1"));

    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("test").expect("agent type"),
            "1.0.0".to_string(),
            "test@1.0.0".to_string(),
        ))
        .await
        .expect("agent boot");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task execution started");
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            msg.clone(),
            "user".to_string(),
            vec!["mixed supersession test".to_string()],
            None,
            agent_id,
            1_700_000_000_601,
        ))
        .await
        .expect("message received");

    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "seed intent".to_string(),
            vec![msg.clone()],
            None,
        ))
        .await
        .expect("intent v1");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "plan-v1".to_string(),
            vec![],
            None,
        ))
        .await
        .expect("plan v1");

    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "refined intent".to_string(),
            vec![msg.clone()],
            Some(PlanningSupersessionKind::RefinedBy),
        ))
        .await
        .expect("intent v2");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "plan-v2".to_string(),
            vec![],
            Some(PlanningSupersessionKind::RefinedBy),
        ))
        .await
        .expect("plan v2");

    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v3".to_string(),
            "replacement intent".to_string(),
            vec![msg],
            Some(PlanningSupersessionKind::ReplacedBy),
        ))
        .await
        .expect("intent v3");
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v3".to_string(),
            "plan-v3".to_string(),
            vec![],
            Some(PlanningSupersessionKind::ReplacedBy),
        ))
        .await
        .expect("plan v3");

    let current_intent = store
        .query_current_intent(&task_id)
        .await
        .expect("query current intent")
        .expect("current intent exists");
    let current_plan = store
        .query_current_plan(&task_id)
        .await
        .expect("query current plan")
        .expect("current plan exists");
    assert_eq!(current_intent.intent_id, "intent-v3");
    assert_eq!(
        current_intent.supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(current_intent.superseded_by_next, None);
    assert_eq!(current_plan.plan_id, "plan-v3");
    assert_eq!(current_plan.intent_id, "intent-v3");
    assert_eq!(
        current_plan.supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(current_plan.superseded_by_next, None);

    let intent_history = store
        .query_intent_history(&task_id, Some(10))
        .await
        .expect("query intent history");
    assert_eq!(intent_history.len(), 3);
    assert_eq!(intent_history[0].intent_id, "intent-v3");
    assert_eq!(
        intent_history[0].supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(intent_history[0].superseded_by_next, None);
    assert_eq!(intent_history[1].intent_id, "intent-v2");
    assert_eq!(
        intent_history[1].supersession_from_previous,
        Some(PlanningSupersessionKind::RefinedBy)
    );
    assert_eq!(
        intent_history[1].superseded_by_next,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(intent_history[2].intent_id, "intent-v1");
    assert_eq!(intent_history[2].supersession_from_previous, None);
    assert_eq!(
        intent_history[2].superseded_by_next,
        Some(PlanningSupersessionKind::RefinedBy)
    );

    let plan_history = store
        .query_plan_history(&task_id, Some(10))
        .await
        .expect("query plan history");
    assert_eq!(plan_history.len(), 3);
    assert_eq!(plan_history[0].plan_id, "plan-v3");
    assert_eq!(
        plan_history[0].supersession_from_previous,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(plan_history[0].superseded_by_next, None);
    assert_eq!(plan_history[1].plan_id, "plan-v2");
    assert_eq!(
        plan_history[1].supersession_from_previous,
        Some(PlanningSupersessionKind::RefinedBy)
    );
    assert_eq!(
        plan_history[1].superseded_by_next,
        Some(PlanningSupersessionKind::ReplacedBy)
    );
    assert_eq!(plan_history[2].plan_id, "plan-v1");
    assert_eq!(plan_history[2].supersession_from_previous, None);
    assert_eq!(
        plan_history[2].superseded_by_next,
        Some(PlanningSupersessionKind::RefinedBy)
    );
}

/// Reproduce the ClickUp agent bug: tool calls written to graphqlite must
/// appear in conversation_context as tool_call + tool_result items.
#[tokio::test]
async fn graphqlite_conversation_context_includes_tool_calls() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(42, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-tool-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());

    // 1) AgentBooted + TaskExists + TaskExecutionStarted (required bootstrap)
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(100),
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
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");

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
                agent_id: agent_id.clone(),
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
            None,
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
            None,
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
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(43, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-tool-failure"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000098").unwrap());

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(200),
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
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
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
                agent_id: agent_id.clone(),
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
            None,
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
            None,
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
async fn graphqlite_conversation_context_preserves_large_tool_result_payload() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(55, 1);
    let task_id = TaskId::from_external(ExternalId::new("task-tool-large"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000096").unwrap());

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(240),
            timestamp_ms: 1_700_000_000_200,
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
            id: EventId::from_counter(241),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_201,
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");

    let tool_args = serde_json::json!({"list_id":"901325431486"});
    let very_large_description = "d".repeat(12_000);
    let completed_metadata = serde_json::json!({
        "message_id": "msg-large",
        "task_id": "task-tool-large",
        "agent_id": "00000000-0000-0000-0000-000000000096",
        "phase": "send",
        "result": {
            "tasks": [{
                "id": "86afp6yhu",
                "name": "Task30",
                "status": "to do",
                "description": very_large_description,
                "url": "https://app.clickup.com/t/86afp6yhu"
            }],
            "items": [],
            "message": "Found 1 task(s)"
        }
    });
    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id,
            "support/clickup".to_string(),
            None,
            tool_args,
            completed_metadata,
            99,
            baml_rt_core::Outcome::Success,
            None,
        ))
        .await
        .expect("ToolCallCompleted");

    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    let sources: Vec<_> = items.iter().map(|i| i.source.clone()).collect();
    assert!(
        sources.iter().any(|s| s == "tool_result"),
        "expected tool_result in conversation_context; sources={sources:?}"
    );
    let tool_result_item = items
        .iter()
        .find(|i| i.source == "tool_result")
        .expect("tool_result item");
    let description = tool_result_item
        .content
        .get("result")
        .and_then(|v| v.get("tasks"))
        .and_then(|v| v.as_array())
        .and_then(|tasks| tasks.first())
        .and_then(|task| task.get("description"))
        .and_then(|v| v.as_str())
        .expect("task description");

    assert_eq!(
        description.len(),
        12_000,
        "tool result description should not be truncated"
    );
}

#[tokio::test]
async fn graphqlite_tool_call_writes_enforce_args_edge_role_and_type() {
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build store");

    let context_id = ContextId::new(77, 9);
    let task_id = TaskId::from_external(ExternalId::new("task-contract-1"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000097").unwrap());

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(300),
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
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");

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
            None,
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
             RETURN used.prov_role AS role, args.prov_type AS target_type \
             ORDER BY t.a2a_event_id LIMIT 1",
            &params,
        )
        .await
        .expect("query args-edge contract row");
    let row = rows
        .iter()
        .next()
        .expect("at least one ToolCall->ToolArgs edge");

    // Build a stable map for snapshot comparison; exclude rel_props (includes internal node IDs).
    let edge_role: Option<String> = row.get("role").ok();
    let node_type: Option<String> = row.get("target_type").ok();
    let edge_snapshot = serde_json::json!({
        "role": edge_role,
        "target_type": node_type,
    });
    assert_json_snapshot!("tool_call_args_edge_row", edge_snapshot);
}
