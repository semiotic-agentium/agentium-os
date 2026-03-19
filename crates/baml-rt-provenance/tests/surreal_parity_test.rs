//! Backend parity tests: scenarios validate behavioral equivalence of the SurrealDB
//! provenance store.
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


use std::sync::Arc;

use baml_rt_core::{
    Outcome,
    ids::{
        AgentId, ArtifactId, ContextId, EventId, ExternalId, MessageId, PlanStepId, TaskId, UuidId,
    },
};
use baml_rt_provenance::{
    AgentBootedEvent, AgentType, CallScope, LlmUsage, PlanStepSpec, ProvEvent, ProvEventData,
    ProvenanceContextReader, ProvenanceOpsQuery, ProvenancePlanningQuery, ProvenanceQueryApi,
    ProvenanceWriter, SurrealStoreBuilder, TaskScopedEvent,
};
use baml_rt_vocabulary::A2aGraphStore;

// ---------------------------------------------------------------------------
// Unified store trait object — all query traits the parity tests need
// ---------------------------------------------------------------------------

/// Combined trait for parity assertions. SurrealDB implements all of these.
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
    assert_eq!(items[0].source_name(), "message");
    assert_eq!(items[1].role, "ROLE_AGENT");
    assert_eq!(items[1].source_name(), "message");

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
        .filter(|i| i.source_name() == "tool_call" || i.source_name() == "tool_result")
        .collect();
    assert!(
        !tool_items.is_empty(),
        "should have tool call items in conversation context"
    );

    use baml_rt_provenance::store::ConversationItemContent;
    let has_failed_tool_result = items.iter().any(|i| {
        matches!(
            &i.content,
            ConversationItemContent::ToolResult(tr) if matches!(&tr.outcome, baml_rt_provenance::store::ToolOutcome::Error(_))
        )
    });
    assert!(
        has_failed_tool_result,
        "conversation context should include failed tool results"
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
            None,
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

// NOTE: A2A graph traversals are tested separately for the SurrealDB backend.
// This test validates the A2aGraphStore trait implementation.
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

// ===========================================================================
// Scenario 11: Failed call has failure classification (Gap 4 / Gap 13)
// ===========================================================================

async fn ops_failed_call_has_failure_classification(store: &dyn ParityStore) {
    bootstrap(store, 1100).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    // Failed LLM call with error in metadata (will be classified)
    store
        .add_event(ProvEvent::llm_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "anthropic".to_string(),
            "claude-3".to_string(),
            "classify".to_string(),
            serde_json::json!({"prompt": "test"}),
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
                "error": "rate limit exceeded"
            }),
            LlmUsage::Unknown,
            100,
            Outcome::Failure,
        ))
        .await
        .expect("failed llm_call_completed");

    // Failed tool call with timeout error
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "web_search".to_string(),
            None,
            serde_json::json!({"query": "test"}),
            serde_json::json!({
                "phase": "send",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "web_search".to_string(),
            None,
            serde_json::json!({"query": "test"}),
            serde_json::json!({
                "phase": "send",
                "error": "timeout connecting to server",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            200,
            Outcome::Failure,
            None,
        ))
        .await
        .expect("failed tool_call_completed");

    // Query LLM calls with FailedOnly filter
    let llm_request = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::LlmCalls,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            ..Default::default()
        },
        outcome: Some(baml_rt_provenance::store::ProvenanceOutcomeSegment::FailedOnly),
        page_size: Some(50),
        ..Default::default()
    };
    let llm_response = store
        .query_ops(llm_request)
        .await
        .expect("query_ops LlmCalls");
    assert!(
        !llm_response.rows.is_empty(),
        "should have failed LLM call rows"
    );

    // Verify failure classification fields are present
    let llm_row = &llm_response.rows[0];
    let failure_class = llm_row.get("failure_class").and_then(|v| v.as_str());
    let failure_evidence = llm_row.get("failure_evidence").and_then(|v| v.as_str());
    assert!(
        failure_class.is_some(),
        "failed LLM call should have failure_class, got row: {:?}",
        llm_row
    );
    assert!(
        failure_evidence.is_some(),
        "failed LLM call should have failure_evidence"
    );

    // Query tool calls with FailedOnly filter
    let tool_request = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::ToolCalls,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            ..Default::default()
        },
        outcome: Some(baml_rt_provenance::store::ProvenanceOutcomeSegment::FailedOnly),
        page_size: Some(50),
        ..Default::default()
    };
    let tool_response = store
        .query_ops(tool_request)
        .await
        .expect("query_ops ToolCalls");
    assert!(
        !tool_response.rows.is_empty(),
        "should have failed tool call rows"
    );

    let tool_row = &tool_response.rows[0];
    let tool_failure_class = tool_row.get("failure_class").and_then(|v| v.as_str());
    let tool_failure_evidence = tool_row.get("failure_evidence").and_then(|v| v.as_str());
    assert!(
        tool_failure_class.is_some(),
        "failed tool call should have failure_class, got row: {:?}",
        tool_row
    );
    assert!(
        tool_failure_evidence.is_some(),
        "failed tool call should have failure_evidence"
    );
}

parity_test!(ops_failed_call_has_failure_classification);

// ===========================================================================
// Scenario 12: Messages ignore task_id filter (Gap 7)
// ===========================================================================

async fn ops_messages_task_filter_parity(store: &dyn ParityStore) {
    // Use a fresh context for this test to avoid interference
    let context_id = ContextId::new(7, 7);
    let task_id_a = TaskId::from_external(ExternalId::new("task-msg-filter-a"));
    let task_id_b = TaskId::from_external(ExternalId::new("task-msg-filter-b"));
    let agent_id = test_agent_id();

    // Bootstrap for this context
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(1200),
            timestamp_ms: 1_700_000_001_200,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("AgentBooted");

    // Task A setup
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1201),
            context_id: context_id.clone(),
            task_id: task_id_a.clone(),
            timestamp_ms: 1_700_000_001_201,
            data: ProvEventData::TaskExists {
                task_id: task_id_a.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists A");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1202),
            context_id: context_id.clone(),
            task_id: task_id_a.clone(),
            timestamp_ms: 1_700_000_001_202,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id_a.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted A");

    // Task B setup
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1203),
            context_id: context_id.clone(),
            task_id: task_id_b.clone(),
            timestamp_ms: 1_700_000_001_203,
            data: ProvEventData::TaskExists {
                task_id: task_id_b.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists B");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1204),
            context_id: context_id.clone(),
            task_id: task_id_b.clone(),
            timestamp_ms: 1_700_000_001_204,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id_b.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted B");

    // Message in Task A
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1210),
            context_id: context_id.clone(),
            task_id: task_id_a.clone(),
            timestamp_ms: 1_700_000_001_210,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-task-a")),
                role: "user".to_string(),
                content: vec!["Message in Task A".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("MessageReceived in Task A");

    // Message in Task B
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1211),
            context_id: context_id.clone(),
            task_id: task_id_b.clone(),
            timestamp_ms: 1_700_000_001_211,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-task-b")),
                role: "user".to_string(),
                content: vec!["Message in Task B".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("MessageReceived in Task B");

    // Query Messages with task_id filter for Task A - should return ALL messages in context
    // (Messages resource ignores task_id filter — returns all messages in context)
    let request_with_task_filter = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::Messages,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            task_id: Some(task_id_a.clone()),
            ..Default::default()
        },
        page_size: Some(50),
        ..Default::default()
    };
    let response_with_filter = store
        .query_ops(request_with_task_filter)
        .await
        .expect("query_ops Messages with task_id");

    // Query Messages without task_id filter
    let request_without_task_filter = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::Messages,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            ..Default::default()
        },
        page_size: Some(50),
        ..Default::default()
    };
    let response_without_filter = store
        .query_ops(request_without_task_filter)
        .await
        .expect("query_ops Messages without task_id");

    // Both should return the same number of messages (task_id filter is ignored for Messages)
    assert_eq!(
        response_with_filter.rows.len(),
        response_without_filter.rows.len(),
        "Messages resource should ignore task_id filter: with_filter={}, without_filter={}",
        response_with_filter.rows.len(),
        response_without_filter.rows.len()
    );
    assert_eq!(
        response_with_filter.rows.len(),
        2,
        "Should have 2 messages in context"
    );
}

parity_test!(ops_messages_task_filter_parity);

// ===========================================================================
// Scenario 13: Empty payload_text returns all rows (Gap 8)
// ===========================================================================

async fn ops_payload_text_empty_query_no_filter(store: &dyn ParityStore) {
    // Use unique context to avoid interference from other tests
    let context_id = ContextId::new(13, 13);
    let task_id = TaskId::from_external(ExternalId::new("task-payload-text"));
    let agent_id = test_agent_id();

    // Bootstrap for this context
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(1300),
            timestamp_ms: 1_700_000_001_300,
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
            id: EventId::from_counter(1301),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_301,
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1302),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_302,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted");

    // Add LLM calls using Started+Completed pairs to get unique ordinals
    // (LlmCallCompleted without LlmCallStarted reuses ordinal 0)
    for i in 0u64..3 {
        let base_ts = 1_700_000_001_310 + i * 10;
        // LlmCallStarted - increments ordinal counter
        store
            .add_event(ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(1310 + i * 2),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: base_ts,
                data: ProvEventData::LlmCallStarted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    client: "anthropic".to_string(),
                    model: "claude-3".to_string(),
                    function_name: format!("function_{}", i),
                    prompt: serde_json::json!({"prompt": format!("prompt {}", i)}),
                    metadata: serde_json::json!({
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str()
                    }),
                },
            }))
            .await
            .expect("llm_call_started");

        // LlmCallCompleted - uses ordinal from Started
        store
            .add_event(ProvEvent::Task(TaskScopedEvent {
                id: EventId::from_counter(1311 + i * 2),
                context_id: context_id.clone(),
                task_id: task_id.clone(),
                timestamp_ms: base_ts + 5,
                data: ProvEventData::LlmCallCompleted {
                    scope: CallScope::Task {
                        task_id: task_id.clone(),
                    },
                    client: "anthropic".to_string(),
                    model: "claude-3".to_string(),
                    function_name: format!("function_{}", i),
                    prompt: serde_json::json!({"prompt": format!("prompt {}", i)}),
                    metadata: serde_json::json!({
                        "result": format!("result {}", i),
                        "agent_id": agent_id.as_str(),
                        "task_id": task_id.as_str()
                    }),
                    usage: LlmUsage::Known {
                        prompt_tokens: 10,
                        completion_tokens: 5,
                        total_tokens: 15,
                        cached_input_tokens: None,
                    },
                    duration_ms: 100,
                    outcome: Outcome::Success,
                    drift: None,
                },
            }))
            .await
            .expect("llm_call_completed");
    }

    // Query with empty payload_text (whitespace only)
    let request_empty = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::LlmCalls,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            payload_text: Some("   ".to_string()), // whitespace only
            ..Default::default()
        },
        page_size: Some(50),
        ..Default::default()
    };
    let response_empty = store
        .query_ops(request_empty)
        .await
        .expect("query_ops with empty payload_text");

    // Query without payload_text filter
    let request_none = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::LlmCalls,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            ..Default::default()
        },
        page_size: Some(50),
        ..Default::default()
    };
    let response_none = store
        .query_ops(request_none)
        .await
        .expect("query_ops without payload_text");

    // Empty/whitespace payload_text should act as no-op filter (return all rows)
    assert_eq!(
        response_empty.rows.len(),
        response_none.rows.len(),
        "empty payload_text should not filter: empty={}, none={}",
        response_empty.rows.len(),
        response_none.rows.len()
    );
    assert!(
        response_empty.rows.len() >= 3,
        "should have at least 3 LLM call rows, got {}: {:?}",
        response_empty.rows.len(),
        response_empty.rows
    );
}

parity_test!(ops_payload_text_empty_query_no_filter);

// ===========================================================================
// Scenario 14: Tool open phase excluded from ToolCalls (Gap 5)
// ===========================================================================

async fn ops_tool_open_phase_excluded(store: &dyn ParityStore) {
    bootstrap(store, 1400).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    // Tool call with phase=open (should be excluded)
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "session_tool".to_string(),
            Some("start".to_string()),
            serde_json::json!({"session": "open"}),
            serde_json::json!({
                "phase": "open",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started open");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "session_tool".to_string(),
            Some("start".to_string()),
            serde_json::json!({"session": "open"}),
            serde_json::json!({
                "phase": "open",
                "result": "session opened",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            50,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed open");

    // Tool call with phase=send (should be included)
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "session_tool".to_string(),
            Some("action".to_string()),
            serde_json::json!({"action": "do_something"}),
            serde_json::json!({
                "phase": "send",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started send");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "session_tool".to_string(),
            Some("action".to_string()),
            serde_json::json!({"action": "do_something"}),
            serde_json::json!({
                "phase": "send",
                "result": "action done",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            100,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed send");

    // Tool call with phase=finish (should be included)
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "session_tool".to_string(),
            Some("end".to_string()),
            serde_json::json!({"session": "close"}),
            serde_json::json!({
                "phase": "finish",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started finish");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "session_tool".to_string(),
            Some("end".to_string()),
            serde_json::json!({"session": "close"}),
            serde_json::json!({
                "phase": "finish",
                "result": "session closed",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            50,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed finish");

    // Query ToolCalls
    let request = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::ToolCalls,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id.clone()),
            ..Default::default()
        },
        page_size: Some(50),
        ..Default::default()
    };
    let response = store.query_ops(request).await.expect("query_ops ToolCalls");

    // Should have 2 rows (send + finish), not 3 (open should be excluded)
    assert_eq!(
        response.rows.len(),
        2,
        "ToolCalls should exclude phase=open rows, got {} rows: {:?}",
        response.rows.len(),
        response
            .rows
            .iter()
            .map(|r| r.get("phase"))
            .collect::<Vec<_>>()
    );

    // Verify no row has phase=open
    for row in &response.rows {
        let phase = row
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_ne!(
            phase, "open",
            "ToolCalls should not include phase=open rows"
        );
    }
}

parity_test!(ops_tool_open_phase_excluded);

// ===========================================================================
// Scenario 15: Supersession cross-task contamination guard (Gap 5 supersession)
// ===========================================================================

async fn supersession_cross_task_contamination_guard(store: &dyn ParityStore) {
    // Use fresh context to avoid interference
    let context_id = ContextId::new(15, 15);
    let task_id_a = TaskId::from_external(ExternalId::new("task-super-a"));
    let task_id_b = TaskId::from_external(ExternalId::new("task-super-b"));
    let agent_id = test_agent_id();
    let msg_1 = MessageId::from_external(ExternalId::new("msg-super-1"));

    // Bootstrap
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(1500),
            timestamp_ms: 1_700_000_001_500,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("AgentBooted");

    // Task A setup
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1501),
            context_id: context_id.clone(),
            task_id: task_id_a.clone(),
            timestamp_ms: 1_700_000_001_501,
            data: ProvEventData::TaskExists {
                task_id: task_id_a.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists A");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1502),
            context_id: context_id.clone(),
            task_id: task_id_a.clone(),
            timestamp_ms: 1_700_000_001_502,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id_a.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted A");

    // Task B setup
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1503),
            context_id: context_id.clone(),
            task_id: task_id_b.clone(),
            timestamp_ms: 1_700_000_001_503,
            data: ProvEventData::TaskExists {
                task_id: task_id_b.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists B");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1504),
            context_id: context_id.clone(),
            task_id: task_id_b.clone(),
            timestamp_ms: 1_700_000_001_504,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id_b.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted B");

    // Message for intent derivation (in Task A)
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1510),
            context_id: context_id.clone(),
            task_id: task_id_a.clone(),
            timestamp_ms: 1_700_000_001_510,
            data: ProvEventData::MessageReceived {
                id: msg_1.clone(),
                role: "user".to_string(),
                content: vec!["do something".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("message");

    // Intent v1 in Task A
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id_a.clone(),
            "intent-a-v1".to_string(),
            "Intent A v1".to_string(),
            vec![msg_1.clone()],
            None,
            None,
        ))
        .await
        .expect("intent a v1");

    // Intent v2 in Task A (supersedes v1)
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id_a.clone(),
            "intent-a-v2".to_string(),
            "Intent A v2".to_string(),
            vec![msg_1.clone()],
            Some(baml_rt_core::bus::PlanningSupersessionKind::ReplacedBy),
            None,
        ))
        .await
        .expect("intent a v2");

    // Intent v1 in Task B (independent, not superseded)
    store
        .add_event(ProvEvent::intent_resolved(
            context_id.clone(),
            task_id_b.clone(),
            "intent-b-v1".to_string(),
            "Intent B v1".to_string(),
            vec![msg_1.clone()],
            None,
            None,
        ))
        .await
        .expect("intent b v1");

    // Query current intent for Task A - should be v2
    let current_a = store
        .query_current_intent(&task_id_a)
        .await
        .expect("query_current_intent A");
    assert!(current_a.is_some(), "Task A should have current intent");
    assert_eq!(
        current_a.as_ref().unwrap().intent_id,
        "intent-a-v2",
        "Task A current intent should be v2"
    );

    // Query current intent for Task B - should be v1 (not affected by Task A supersession)
    let current_b = store
        .query_current_intent(&task_id_b)
        .await
        .expect("query_current_intent B");
    assert!(current_b.is_some(), "Task B should have current intent");
    assert_eq!(
        current_b.as_ref().unwrap().intent_id,
        "intent-b-v1",
        "Task B current intent should be v1 (not affected by Task A supersession)"
    );

    // Query intent history for Task A - should have 2
    let history_a = store
        .query_intent_history(&task_id_a, Some(10))
        .await
        .expect("query_intent_history A");
    assert_eq!(
        history_a.len(),
        2,
        "Task A should have 2 intents in history"
    );

    // Query intent history for Task B - should have 1
    let history_b = store
        .query_intent_history(&task_id_b, Some(10))
        .await
        .expect("query_intent_history B");
    assert_eq!(history_b.len(), 1, "Task B should have 1 intent in history");
}

parity_test!(supersession_cross_task_contamination_guard);

// ===========================================================================
// Scenario 16: Conversation context skips rows with missing required fields (Gap 11)
// ===========================================================================

async fn conversation_context_required_fields_skip(store: &dyn ParityStore) {
    bootstrap(store, 1600).await;
    let context_id = test_context_id();
    let task_id = test_task_id();
    let agent_id = test_agent_id();

    // Add a valid message first
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1610),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_610,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-valid")),
                role: "user".to_string(),
                content: vec!["Valid message".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
            },
        }))
        .await
        .expect("valid message");

    // Add a valid tool call
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "calculator".to_string(),
            Some("add".to_string()),
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({
                "phase": "send",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "calculator".to_string(),
            Some("add".to_string()),
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({
                "phase": "send",
                "result": 3,
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            100,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");

    // Query conversation context
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    // Should have items (at least the message and tool call/result)
    assert!(
        !items.is_empty(),
        "conversation_context should have valid items"
    );

    // All items should have non-empty event_id and appropriate source
    use baml_rt_provenance::store::{ConversationItemContent as CIC, ToolCallContent as TCC, ToolResultContent as TRC};
    for item in &items {
        assert!(
            !item.event_id.as_str().is_empty(),
            "item should have non-empty event_id"
        );
        assert!(!item.source_name().is_empty(), "item should have non-empty source");
        if let CIC::ToolCall(tc) = &item.content {
            assert!(
                !tc.tool_name.is_empty(),
                "tool_call item should have non-empty tool_name"
            );
        }
        if let CIC::ToolResult(tr) = &item.content {
            assert!(
                !tr.tool_name.is_empty(),
                "tool_result item should have non-empty tool_name"
            );
        }
    }
}

parity_test!(conversation_context_required_fields_skip);

// ===========================================================================
// Scenario 17: Conversation context tool metadata fallback (Gap 10)
// ===========================================================================

async fn conversation_context_tool_metadata_fallback(store: &dyn ParityStore) {
    // Use unique context for this test
    let context_id = ContextId::new(17, 17);
    let task_id = TaskId::from_external(ExternalId::new("task-metadata-fallback"));
    let agent_id = test_agent_id();

    // Bootstrap
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(1700),
            timestamp_ms: 1_700_000_001_700,
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
            id: EventId::from_counter(1701),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_701,
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1702),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_702,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted");

    // Add tool call with metadata containing args/result/error info
    // The a2a_metadata in props should be used as fallback when payloads are missing
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "test_tool".to_string(),
            Some("action".to_string()),
            serde_json::json!({"input": "test_value"}), // args
            serde_json::json!({
                "phase": "send",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
                // Metadata that could be used for fallback
                "a2a_args": {"input": "test_value"},
                "a2a_phase": "send"
            }),
            None,
        ))
        .await
        .expect("tool_call_started");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "test_tool".to_string(),
            Some("action".to_string()),
            serde_json::json!({"input": "test_value"}),
            serde_json::json!({
                "phase": "send",
                "result": {"output": "success"},
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
                // Metadata for fallback
                "a2a_result": {"output": "success"},
                "a2a_phase": "send"
            }),
            100,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");

    // Query conversation context
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    // Should have tool_call and tool_result items
    let tool_call_items: Vec<_> = items.iter().filter(|i| i.source_name() == "tool_call").collect();
    let tool_result_items: Vec<_> = items.iter().filter(|i| i.source_name() == "tool_result").collect();

    assert!(!tool_call_items.is_empty(), "should have tool_call items");
    assert!(
        !tool_result_items.is_empty(),
        "should have tool_result items"
    );

    // Verify tool_call has args
    let tool_call = tool_call_items[0];
    if let baml_rt_provenance::store::ConversationItemContent::ToolCall(tc) = &tool_call.content {
        assert!(!serde_json::to_string(&tc.args).unwrap_or_default().is_empty(), "tool_call should have args");
    } else {
        panic!("expected ToolCall variant");
    }

    // Verify tool_result has meaningful outcome
    let tool_result = tool_result_items[0];
    if let baml_rt_provenance::store::ConversationItemContent::ToolResult(tr) = &tool_result.content {
        assert!(!matches!(&tr.outcome, baml_rt_provenance::store::ToolOutcome::StatusOnly), "tool_result should have result or error");
    } else {
        panic!("expected ToolResult variant");
    }
}

parity_test!(conversation_context_tool_metadata_fallback);

// ===========================================================================
// Scenario 18: Conversation context contract filtering (Gap 9)
// This tests that tool calls with proper edge topology are included
// ===========================================================================

async fn conversation_context_contract_filtering(store: &dyn ParityStore) {
    // Use unique context
    let context_id = ContextId::new(18, 18);
    let task_id = TaskId::from_external(ExternalId::new("task-contract-filter"));
    let agent_id = test_agent_id();

    // Bootstrap
    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: EventId::from_counter(1800),
            timestamp_ms: 1_700_000_001_800,
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
            id: EventId::from_counter(1801),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_801,
            data: ProvEventData::TaskExists {
                task_id: task_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExists");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: EventId::from_counter(1802),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_001_802,
            data: ProvEventData::TaskExecutionStarted {
                task_id: task_id.clone(),
                agent_id: agent_id.clone(),
                context_id: context_id.clone(),
            },
        }))
        .await
        .expect("TaskExecutionStarted");

    // Add a valid tool call with proper Started+Completed pair
    // This creates proper ToolCall -> USED -> ToolArgs edge topology
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "valid_tool".to_string(),
            Some("action".to_string()),
            serde_json::json!({"arg1": "value1"}),
            serde_json::json!({
                "phase": "send",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "valid_tool".to_string(),
            Some("action".to_string()),
            serde_json::json!({"arg1": "value1"}),
            serde_json::json!({
                "phase": "send",
                "result": "done",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            100,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");

    // Query conversation context
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");

    // Should have tool_call and tool_result items for the valid tool
    let tool_call_items: Vec<_> = items.iter().filter(|i| i.source_name() == "tool_call").collect();

    // The valid tool should be included (has proper edge topology)
    assert!(
        !tool_call_items.is_empty(),
        "valid tool_call with proper edge contract should be included"
    );

    // Verify the tool name
    let has_valid_tool = tool_call_items.iter().any(|item| {
        if let baml_rt_provenance::store::ConversationItemContent::ToolCall(tc) = &item.content {
            tc.tool_name == "valid_tool"
        } else {
            false
        }
    });
    assert!(
        has_valid_tool,
        "valid_tool should be in conversation context"
    );
}

parity_test!(conversation_context_contract_filtering);
