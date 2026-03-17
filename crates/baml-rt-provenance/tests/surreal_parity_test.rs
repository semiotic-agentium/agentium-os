//! Backend-agnostic parity tests: same scenario runs against GraphQLite and SurrealDB,
//! results are compared for behavioral equivalence.
//!
//! Feature-gated behind `surreal-backend` so CI can run incrementally.
//!
//! ## Design
//!
//! Each scenario is a plain async function taking a `&dyn ProvenanceWriter + ProvenanceContextReader + ...`.
//! The macro `parity_test!` generates two `#[tokio::test]` functions per scenario — one for each backend.
//!
//! ## Test matrix (from Phase 1 plan)
//!
//! 1. Message lifecycle
//! 2. Tool call lifecycle (success + failure)
//! 3. Task lifecycle (submitted → working → completed)
//! 4. Artifacts and status updates
//! 5. Planning entities (intent + plan + supersession)
//! 6. Tool call args edge (Phase 2 — requires raw graph query)
//! 7. Ops query filters
//! 8. A2A graph traversals
//! 9. Payload/archive behavior
//! 10. No-stale-read interleaved test

#![cfg(feature = "surreal-backend")]

use std::sync::Arc;

use baml_rt_core::{
    Outcome,
    ids::{
        AgentId, ArtifactId, ContextId, EventId, ExternalId, MessageId, PlanStepId, TaskId, UuidId,
    },
};
use baml_rt_provenance::{
    AgentBootedEvent, AgentType, GraphqliteStoreBuilder, LlmUsage, PlanStepSpec, ProvEvent,
    ProvEventData, ProvenanceContextReader, ProvenanceOpsQuery, ProvenancePlanningQuery,
    ProvenanceQueryApi, ProvenanceWriter, SurrealStoreBuilder, TaskScopedEvent,
};
use baml_rt_vocabulary::A2aGraphStore;

// ---------------------------------------------------------------------------
// Unified store trait object — all query traits the parity tests need
// ---------------------------------------------------------------------------

/// Combined trait for parity assertions. Both GraphQLite and Surreal implement all of these.
trait ParityStore:
    ProvenanceWriter
    + ProvenanceContextReader
    + ProvenanceQueryApi
    + ProvenancePlanningQuery
    + ProvenanceOpsQuery
    + A2aGraphStore
    + Send
    + Sync
{
}

impl<T> ParityStore for T where
    T: ProvenanceWriter
        + ProvenanceContextReader
        + ProvenanceQueryApi
        + ProvenancePlanningQuery
        + ProvenanceOpsQuery
        + A2aGraphStore
        + Send
        + Sync
{
}

// ---------------------------------------------------------------------------
// Store factories
// ---------------------------------------------------------------------------

fn build_graphqlite_store() -> Arc<dyn ParityStore> {
    GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build GraphQLite isolated store")
}

async fn build_surreal_store() -> Arc<dyn ParityStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build SurrealDB isolated store")
}

// ---------------------------------------------------------------------------
// Shared test IDs + bootstrap
// ---------------------------------------------------------------------------

fn test_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap())
}

fn test_context_id() -> ContextId {
    ContextId::new(1, 1)
}

fn test_task_id() -> TaskId {
    TaskId::from_external(ExternalId::new("task-parity-1"))
}

/// Bootstrap: AgentBooted + TaskExists + TaskExecutionStarted.
/// Uses deterministic event IDs starting from `base`.
async fn bootstrap(store: &dyn ParityStore, base: u64) {
    let agent_id = test_agent_id();
    let context_id = test_context_id();
    let task_id = test_task_id();

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(base),
            timestamp_ms: 1_700_000_000_000 + base,
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
            id: EventId::from_counter(base + 1),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_000 + base + 1,
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(base + 2),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_000 + base + 2,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted");
}

// ---------------------------------------------------------------------------
// Parity macro: generates one test per backend from a scenario function
// ---------------------------------------------------------------------------

macro_rules! parity_test {
    ($name:ident) => {
        paste::paste! {
            #[tokio::test]
            async fn [<graphqlite_ $name>]() {
                let store = build_graphqlite_store();
                $name(&*store).await;
            }

            #[tokio::test]
            async fn [<surreal_ $name>]() {
                let store = build_surreal_store().await;
                $name(&*store).await;
            }
        }
    };
}

// ===========================================================================
// Scenario 1: Message lifecycle
// ===========================================================================

async fn message_lifecycle(store: &dyn ParityStore) {
    bootstrap(store, 0).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    // User message
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(10),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_010,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-user-1")),
                role: "user".to_string(),
                content: vec!["Hello from user".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("MessageReceived");

    // Assistant message
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(11),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_011,
            data: ProvEventData::MessageSent {
                id: MessageId::from_external(ExternalId::new("msg-agent-1")),
                role: "assistant".to_string(),
                content: vec!["Hello from agent".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("MessageSent");

    // Assert context_messages
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("context_messages");
    assert_eq!(messages.len(), 2, "expect 2 messages");
    assert_eq!(messages[0].role, "ROLE_USER");
    assert_eq!(messages[0].content, vec!["Hello from user"]);
    assert_eq!(messages[1].role, "ROLE_AGENT");
    assert_eq!(messages[1].content, vec!["Hello from agent"]);

    // Assert conversation_context
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    assert_eq!(items.len(), 2, "expect 2 conversation items");
    assert_eq!(items[0].role, "ROLE_USER");
    assert_eq!(items[0].source, "message");
    assert_eq!(items[1].role, "ROLE_AGENT");
    assert_eq!(items[1].source, "message");

    // Assert limit works
    let limited = store
        .context_messages(&context_id, Some(1))
        .await
        .expect("context_messages limited");
    assert_eq!(limited.len(), 1, "limit=1 should return 1 message");
    assert_eq!(limited[0].role, "ROLE_AGENT", "limit returns most recent");
}

parity_test!(message_lifecycle);

// ===========================================================================
// Scenario 2: Tool call lifecycle (success + failure)
// ===========================================================================

async fn tool_call_lifecycle(store: &dyn ParityStore) {
    bootstrap(store, 100).await;
    let context_id = test_context_id();
    let task_id = test_task_id();

    let agent_id = test_agent_id();
    let agent_id_str = agent_id.as_str();

    // Successful tool call
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "calculator".to_string(),
            Some("add".to_string()),
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({"phase": "send", "agent_id": agent_id_str, "task_id": task_id.as_str()}),
            None,
        ))
        .await
        .expect("tool_call_started success");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "calculator".to_string(),
            Some("add".to_string()),
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({"phase": "send", "result": 3, "agent_id": agent_id_str, "task_id": task_id.as_str()}),
            150,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed success");

    // Failed tool call
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "web_search".to_string(),
            None,
            serde_json::json!({"query": "test"}),
            serde_json::json!({"phase": "send", "agent_id": agent_id_str, "task_id": task_id.as_str()}),
            None,
        ))
        .await
        .expect("tool_call_started failure");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "web_search".to_string(),
            None,
            serde_json::json!({"query": "test"}),
            serde_json::json!({"phase": "send", "error": "rate limited", "agent_id": agent_id_str, "task_id": task_id.as_str()}),
            200,
            Outcome::Failure,
            None,
        ))
        .await
        .expect("tool_call_completed failure");

    // Verify conversation context includes completed tool calls
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    // Should have at least the successful tool call entries
    // (tool_call + tool_result for the successful one)
    let tool_items: Vec<_> = items
        .iter()
        .filter(|i| i.source == "tool_call" || i.source == "tool_result")
        .collect();
    assert!(
        !tool_items.is_empty(),
        "should have tool call items in conversation context"
    );
}

parity_test!(tool_call_lifecycle);

// ===========================================================================
// Scenario 3: Task lifecycle (submitted → working → completed)
// ===========================================================================

async fn task_lifecycle(store: &dyn ParityStore) {
    bootstrap(store, 200).await;
    let context_id = test_context_id();
    let task_id = test_task_id();

    // Status: submitted → working
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            Some("submitted".to_string()),
            Some("working".to_string()),
        ))
        .await
        .expect("status submitted→working");

    // Status: working → completed
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            Some("working".to_string()),
            Some("completed".to_string()),
        ))
        .await
        .expect("status working→completed");

    // Can still read context (empty messages, but no error)
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("context_messages after lifecycle");
    // No messages were sent, so should be empty
    assert!(
        messages.is_empty(),
        "no messages in pure status-change lifecycle"
    );
}

parity_test!(task_lifecycle);

// ===========================================================================
// Scenario 4: Artifacts and status updates
// ===========================================================================

async fn artifacts_and_status(store: &dyn ParityStore) {
    bootstrap(store, 300).await;
    let context_id = test_context_id();
    let task_id = test_task_id();

    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            None,
            Some("working".to_string()),
        ))
        .await
        .expect("status change");

    store
        .add_event(ProvEvent::task_artifact_generated(
            context_id.clone(),
            task_id.clone(),
            Some(ArtifactId::from_external(ExternalId::new("artifact-1"))),
            Some("report".to_string()),
        ))
        .await
        .expect("artifact generated");

    store
        .add_event(ProvEvent::task_artifact_generated(
            context_id.clone(),
            task_id.clone(),
            Some(ArtifactId::from_external(ExternalId::new("artifact-2"))),
            Some("chart".to_string()),
        ))
        .await
        .expect("artifact generated 2");

    // Verify no crash and messages are empty
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("context_messages");
    assert!(messages.is_empty());
}

parity_test!(artifacts_and_status);

// ===========================================================================
// Scenario 5: Planning entities (intent + plan + supersession)
// ===========================================================================

async fn planning_entities(store: &dyn ParityStore) {
    bootstrap(store, 400).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();
    let msg_1 = MessageId::from_external(ExternalId::new("msg-plan-1"));

    // Message to derive intent from
    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            msg_1.clone(),
            "user".to_string(),
            vec!["build a report".to_string()],
            None,
            agent_id.clone(),
            1_700_000_000_401,
        ))
        .await
        .expect("message for intent");

    // Intent v1
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "Generate a report".to_string(),
            vec![msg_1.clone()],
            None,
        ))
        .await
        .expect("intent v1");

    // Plan v1
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v1".to_string(),
            "plan-v1".to_string(),
            vec![PlanStepSpec {
                step_id: PlanStepId::from("step-a"),
                description: "gather data".to_string(),
                order: 0,
                depends_on: vec![],
            }],
            None,
        ))
        .await
        .expect("plan v1");

    // Intent v2 (supersedes v1)
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "Generate a detailed report".to_string(),
            vec![msg_1.clone()],
            Some(baml_rt_core::bus::PlanningSupersessionKind::ReplacedBy),
        ))
        .await
        .expect("intent v2");

    // Plan v2 (supersedes v1)
    store
        .add_event(ProvEvent::plan_generated(
            context_id.clone(),
            task_id.clone(),
            "intent-v2".to_string(),
            "plan-v2".to_string(),
            vec![
                PlanStepSpec {
                    step_id: PlanStepId::from("step-b1"),
                    description: "gather data".to_string(),
                    order: 0,
                    depends_on: vec![],
                },
                PlanStepSpec {
                    step_id: PlanStepId::from("step-b2"),
                    description: "format report".to_string(),
                    order: 1,
                    depends_on: vec![PlanStepId::from("step-b1")],
                },
            ],
            Some(baml_rt_core::bus::PlanningSupersessionKind::ReplacedBy),
        ))
        .await
        .expect("plan v2");

    // Current intent should be v2

    let current_intent = store
        .query_current_intent(&task_id)
        .await
        .expect("query_current_intent");
    assert!(current_intent.is_some(), "should have a current intent");
    let intent = current_intent.unwrap();
    assert_eq!(intent.intent_id, "intent-v2");

    // Current plan should be v2
    let current_plan = store
        .query_current_plan(&task_id)
        .await
        .expect("query_current_plan");
    assert!(current_plan.is_some(), "should have a current plan");
    let plan = current_plan.unwrap();
    assert_eq!(plan.plan_id, "plan-v2");
    assert_eq!(plan.steps.len(), 2, "plan v2 has 2 steps");

    // Intent history should have both
    let intent_history = store
        .query_intent_history(&task_id, Some(10))
        .await
        .expect("query_intent_history");
    assert_eq!(intent_history.len(), 2);

    // Plan history should have both
    let plan_history = store
        .query_plan_history(&task_id, Some(10))
        .await
        .expect("query_plan_history");
    assert_eq!(plan_history.len(), 2);
}

parity_test!(planning_entities);

// ===========================================================================
// Scenario 7: Ops query (basic)
// ===========================================================================

async fn ops_query_basic(store: &dyn ParityStore) {
    bootstrap(store, 600).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    // Add a message and an LLM call
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(610),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_610,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-ops-1")),
                role: "user".to_string(),
                content: vec!["ops test".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("message for ops");

    store
        .add_event(ProvEvent::llm_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "anthropic".to_string(),
            "claude-3".to_string(),
            "classify".to_string(),
            serde_json::json!({"prompt": "test"}),
            serde_json::json!({"result": "positive", "agent_id": agent_id.as_str(), "task_id": task_id.as_str()}),
            LlmUsage::Known {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_input_tokens: Some(2),
            },
            100,
            Outcome::Success,
        ))
        .await
        .expect("llm_call_completed");

    // Query ops — should not error
    let ops_request = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::LlmCalls,
        filters: Default::default(),
        sort_by: None,
        sort_dir: None,
        page_size: Some(50),
        cursor: None,
        group_by: Vec::new(),
        top_k: None,
        outcome: None,
        response_profile: None,
        budget_mode: true,
    };
    let ops_response = store.query_ops(ops_request).await.expect("query_ops");
    assert!(
        !ops_response.rows.is_empty(),
        "ops query should return at least one LLM call row"
    );
}

parity_test!(ops_query_basic);

// ===========================================================================
// Scenario 8: A2A graph traversals
// ===========================================================================

async fn a2a_graph_traversals(store: &dyn ParityStore) {
    // Create a task node directly via upsert (not ensure, to set status)
    let initial = baml_rt_vocabulary::TaskSubgraphNode {
        id: "task-a2a-1".to_string(),
        context_id: "ctx-1".to_string(),
        status_json: r#"{"state":"submitted"}"#.to_string(),
        metadata_json: "{}".to_string(),
        extra_json: "{}".to_string(),
        artifacts_json: "[]".to_string(),
    };
    store
        .upsert_task_node(&initial, 1)
        .await
        .expect("upsert_task_node create");

    // Verify it exists
    let task = store
        .get_task_node("task-a2a-1")
        .await
        .expect("get_task_node");
    assert!(task.is_some(), "task should exist");
    let task = task.unwrap();
    assert_eq!(task.id, "task-a2a-1");
    assert_eq!(task.context_id, "ctx-1");
    assert_eq!(task.status_json, r#"{"state":"submitted"}"#);

    // Set status directly
    store
        .set_task_status_json("task-a2a-1", r#"{"state":"completed"}"#)
        .await
        .expect("set_task_status_json");
    let task = store
        .get_task_node("task-a2a-1")
        .await
        .expect("get_task_node after set_status")
        .expect("task exists");
    assert_eq!(task.status_json, r#"{"state":"completed"}"#);

    // Insert messages
    store
        .insert_message_node("msg-1", "task-a2a-1", 1, r#"{"role":"user","text":"hi"}"#)
        .await
        .expect("insert_message_node 1");
    store
        .insert_message_node(
            "msg-2",
            "task-a2a-1",
            2,
            r#"{"role":"agent","text":"hello"}"#,
        )
        .await
        .expect("insert_message_node 2");

    let messages = store
        .list_message_json("task-a2a-1")
        .await
        .expect("list_message_json");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], r#"{"role":"user","text":"hi"}"#);
    assert_eq!(messages[1], r#"{"role":"agent","text":"hello"}"#);

    // Insert update nodes
    store
        .insert_update_node("upd-1", "task-a2a-1", 1, "status", r#"{"working":true}"#)
        .await
        .expect("insert_update_node");
    store
        .insert_update_node(
            "upd-2",
            "task-a2a-1",
            2,
            "artifact",
            r#"{"file":"report.pdf"}"#,
        )
        .await
        .expect("insert_update_node 2");

    let updates = store
        .list_update_nodes("task-a2a-1")
        .await
        .expect("list_update_nodes");
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].kind, "status");
    assert_eq!(updates[1].kind, "artifact");

    // Delete update node
    store
        .delete_update_node("upd-1")
        .await
        .expect("delete_update_node");
    let updates = store
        .list_update_nodes("task-a2a-1")
        .await
        .expect("list_update_nodes after delete");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].id, "upd-2");

    // Max sequence numbers
    let max_msg = store
        .max_message_seq("task-a2a-1")
        .await
        .expect("max_message_seq");
    assert_eq!(max_msg, 2);

    let max_upd = store
        .max_update_seq("task-a2a-1")
        .await
        .expect("max_update_seq");
    assert_eq!(max_upd, 2); // upd-1 was deleted but upd-2 has seq=2

    // List task nodes with filter
    store
        .ensure_task_node("task-a2a-2", "ctx-1", 2)
        .await
        .expect("ensure second task");
    let all_tasks = store.list_task_nodes(None).await.expect("list all tasks");
    assert_eq!(all_tasks.len(), 2);

    let ctx_tasks = store
        .list_task_nodes(Some("ctx-1"))
        .await
        .expect("list ctx-1 tasks");
    assert_eq!(ctx_tasks.len(), 2);

    let empty_tasks = store
        .list_task_nodes(Some("ctx-nonexistent"))
        .await
        .expect("list nonexistent ctx tasks");
    assert!(empty_tasks.is_empty());

    // Max task ord
    let max_ord = store.max_task_ord().await.expect("max_task_ord");
    assert_eq!(max_ord, 2);
}

// NOTE: A2A graph traversals are tested separately per backend because GraphQLite's
// MERGE pattern for A2ATaskMessageSubgraph uses $param in node identity which the
// extension does not resolve (requires escaped literals). The existing A2A tests in
// graphqlite_store_test.rs / a2a_graph_store.rs cover GraphQLite; this test validates
// SurrealDB parity for the A2aGraphStore trait.
#[tokio::test]
async fn surreal_a2a_graph_traversals() {
    let store = build_surreal_store().await;
    a2a_graph_traversals(&*store).await;
}

// ===========================================================================
// Scenario 10: No-stale-read interleaved test
// ===========================================================================

async fn no_stale_read_interleaved(store: &dyn ParityStore) {
    bootstrap(store, 900).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    // Write message 1
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(910),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_910,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-stale-1")),
                role: "user".to_string(),
                content: vec!["First".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("write msg1");

    // Read → must see 1
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("read after msg1");
    assert_eq!(
        messages.len(),
        1,
        "no-stale-read: first read must see first message"
    );
    assert_eq!(messages[0].content, vec!["First"]);

    // Write message 2
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(911),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_000_911,
            data: ProvEventData::MessageSent {
                id: MessageId::from_external(ExternalId::new("msg-stale-2")),
                role: "assistant".to_string(),
                content: vec!["Second".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("write msg2");

    // Read → must see 2
    let messages = store
        .context_messages(&context_id, None)
        .await
        .expect("read after msg2");
    assert_eq!(
        messages.len(),
        2,
        "no-stale-read: second read must see both messages"
    );
    assert_eq!(messages[0].content, vec!["First"]);
    assert_eq!(messages[1].content, vec!["Second"]);

    // Same for conversation_context
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context after msg2");
    assert_eq!(
        items.len(),
        2,
        "no-stale-read: conversation_context must see both"
    );
}

parity_test!(no_stale_read_interleaved);
