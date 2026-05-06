//! Snapshot regression tests for the SurrealDB provenance store.
//!
//! Structured outputs are captured with `insta` after normalizing volatile fields
//! (timestamps, ordering) so diffs stay stable across runs.
//!
//! ```bash
//! cargo test -p baml-rt-provenance --test surreal_snapshot_test
//! cargo insta review
//! ```

use std::sync::Arc;

use baml_rt_core::{
    Outcome,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentBootedEvent, AgentType, CallScope, LlmUsage, ProvEvent, ProvEventData,
    ProvenanceContextReader, ProvenanceOpsQuery, ProvenancePlanningQuery, ProvenanceQueryApi,
    ProvenanceWriter, SurrealStoreBuilder, TaskScopedEvent, serialized_prompt_utf8_len,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Unified store trait object
// ---------------------------------------------------------------------------

trait SnapshotStore:
    ProvenanceWriter
    + ProvenanceContextReader
    + ProvenanceQueryApi
    + ProvenancePlanningQuery
    + ProvenanceOpsQuery
    + Send
    + Sync
{
}

impl<T> SnapshotStore for T where
    T: ProvenanceWriter
        + ProvenanceContextReader
        + ProvenanceQueryApi
        + ProvenancePlanningQuery
        + ProvenanceOpsQuery
        + Send
        + Sync
{
}

// ---------------------------------------------------------------------------
// Store factories
// ---------------------------------------------------------------------------

async fn build_surreal_store() -> Arc<dyn SnapshotStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build SurrealDB isolated store")
}

// ---------------------------------------------------------------------------
// Test IDs + bootstrap
// ---------------------------------------------------------------------------

fn test_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap())
}

fn test_context_id(n: u64) -> ContextId {
    ContextId::new(n, n)
}

fn test_task_id(name: &str) -> TaskId {
    TaskId::from_external(ExternalId::new(name))
}

async fn bootstrap(store: &dyn SnapshotStore, context_id: &ContextId, task_id: &TaskId, base: u64) {
    let agent_id = test_agent_id();

    store
        .add_event(ProvEvent::AgentBooted(AgentBootedEvent {
            id: ActivityAnchorId::from_counter(base),
            timestamp_ms: 1_700_000_000_000 + base,
            data: ProvEventData::AgentBooted {
                agent_id: agent_id.clone(),
                agent_type: AgentType::new("snapshot-test").expect("agent_type"),
                agent_version: "1.0.0".to_string(),
                archive_path: "snapshot-test@1.0.0".to_string(),
            },
        }))
        .await
        .expect("AgentBooted");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: ActivityAnchorId::from_counter(base + 1),
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
            id: ActivityAnchorId::from_counter(base + 2),
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
// Normalization utilities
// ---------------------------------------------------------------------------

/// Normalize ops query response for snapshot comparison.
/// - Sorts rows by activity_id for deterministic ordering
/// - Removes volatile fields (timestamps that vary by run)
/// - Normalizes null vs missing fields
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NormalizedOpsRow {
    activity_id: String,
    context_id: String,
    task_id: Option<String>,
    agent_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    tool_name: Option<String>,
    baml_prompt: Option<String>,
    duration_ms: Option<u64>,
    activity_outcome: Option<String>,
    activity_status: Option<String>,
    activity_kind: Option<String>,
    phase: Option<String>,
    failure_class: Option<String>,
    failure_evidence: Option<String>,
    // Token fields for LLM calls
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

fn normalize_ops_row(row: &Value) -> NormalizedOpsRow {
    NormalizedOpsRow {
        activity_id: row
            .get("activity_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        context_id: row
            .get("context_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        task_id: row.get("task_id").and_then(Value::as_str).map(String::from),
        agent_id: row
            .get("agent_id")
            .and_then(Value::as_str)
            .map(String::from),
        provider: row
            .get("provider")
            .and_then(Value::as_str)
            .map(String::from),
        model: row.get("model").and_then(Value::as_str).map(String::from),
        tool_name: row
            .get("tool_name")
            .and_then(Value::as_str)
            .map(String::from),
        baml_prompt: row
            .get("baml_prompt")
            .and_then(Value::as_str)
            .map(String::from),
        duration_ms: row.get("duration_ms").and_then(Value::as_u64),
        activity_outcome: row
            .get("activity_outcome")
            .and_then(Value::as_str)
            .map(String::from),
        activity_status: row
            .get("activity_status")
            .and_then(Value::as_str)
            .map(String::from),
        activity_kind: row
            .get("activity_kind")
            .and_then(Value::as_str)
            .map(String::from),
        phase: row.get("phase").and_then(Value::as_str).map(String::from),
        failure_class: row
            .get("failure_class")
            .and_then(Value::as_str)
            .map(String::from),
        failure_evidence: row
            .get("failure_evidence")
            .and_then(Value::as_str)
            .map(String::from),
        prompt_tokens: row.get("prompt_tokens").and_then(Value::as_u64),
        completion_tokens: row.get("completion_tokens").and_then(Value::as_u64),
        total_tokens: row.get("total_tokens").and_then(Value::as_u64),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NormalizedOpsResponse {
    resource: String,
    row_count: usize,
    rows: Vec<NormalizedOpsRow>,
    truncated: bool,
}

fn normalize_ops_response(
    response: &baml_rt_provenance::store::ProvenanceOpsQueryResponse,
) -> NormalizedOpsResponse {
    let mut rows: Vec<NormalizedOpsRow> = response.rows.iter().map(normalize_ops_row).collect();
    // Sort by activity_id for deterministic ordering
    rows.sort_by(|a, b| a.activity_id.cmp(&b.activity_id));

    NormalizedOpsResponse {
        resource: format!("{:?}", response.resource),
        row_count: rows.len(),
        rows,
        truncated: response.truncated,
    }
}

/// Normalize conversation context item for snapshot comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NormalizedContextItem {
    role: String,
    source: String,
    content_keys: Vec<String>,
    // For tool_call: extract name and phase
    tool_name: Option<String>,
    tool_phase: Option<String>,
    // For tool_result: extract result presence
    has_result: bool,
    has_error: bool,
}

fn normalize_context_item(
    item: &baml_rt_conversation::view::ProvenanceConversationContextItem,
) -> NormalizedContextItem {
    use baml_rt_conversation::view::{ConversationItemContent, ToolOutcome};

    let (content_keys, tool_name, tool_phase, has_result, has_error) = match &item.content {
        ConversationItemContent::Message { .. } => {
            (vec!["text".to_string()], None, None, false, false)
        }
        ConversationItemContent::ToolCall(tc) => (
            vec![
                "args".to_string(),
                "fsm_phase".to_string(),
                "tool_name".to_string(),
            ],
            Some(tc.tool_name.clone()),
            Some(tc.fsm_phase.label()),
            false,
            false,
        ),
        ConversationItemContent::ToolResult(tr) => (
            vec![
                "fsm_phase".to_string(),
                "outcome".to_string(),
                "tool_name".to_string(),
            ],
            Some(tr.tool_name.clone()),
            Some(tr.fsm_phase.label()),
            matches!(&tr.outcome, ToolOutcome::Result(_)),
            matches!(&tr.outcome, ToolOutcome::Error(_)),
        ),
        ConversationItemContent::SessionStep(ss) => (
            vec!["op".to_string(), "tool_name".to_string()],
            Some(ss.tool_name.clone()),
            None,
            false,
            false,
        ),
    };

    NormalizedContextItem {
        role: item.role.clone(),
        source: item.source_name().to_string(),
        content_keys,
        tool_name,
        tool_phase,
        has_result,
        has_error,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NormalizedConversationContext {
    item_count: usize,
    items: Vec<NormalizedContextItem>,
}

fn normalize_conversation_context(
    items: &[baml_rt_conversation::view::ProvenanceConversationContextItem],
) -> NormalizedConversationContext {
    let normalized: Vec<NormalizedContextItem> = items.iter().map(normalize_context_item).collect();
    // Don't sort — preserve insertion order from the store read.

    NormalizedConversationContext {
        item_count: normalized.len(),
        items: normalized,
    }
}

// ---------------------------------------------------------------------------
// Snapshot macro
// ---------------------------------------------------------------------------

macro_rules! snapshot_test {
    ($name:ident, $setup:expr, $query:expr) => {
        paste::paste! {
            #[tokio::test]
            async fn [<surreal_snapshot_ $name>]() {
                let store = build_surreal_store().await;
                $setup(&*store).await;
                let result = $query(&*store).await;
                insta::assert_json_snapshot!(
                    concat!(stringify!($name), "@surreal"),
                    result
                );
            }
        }
    };
}

// ===========================================================================
// Snapshot Test 1: Failed call with failure classification
// ===========================================================================

async fn setup_failed_call_with_classification(store: &dyn SnapshotStore) {
    let context_id = test_context_id(100);
    let task_id = test_task_id("task-failed-class");
    let agent_id = test_agent_id();

    bootstrap(store, &context_id, &task_id, 10000).await;

    // Failed LLM call
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: ActivityAnchorId::from_counter(10010),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_010_010,
            data: ProvEventData::LlmCallStarted {
                scope: CallScope::Task {
                    task_id: task_id.clone(),
                },
                client: "anthropic".to_string(),
                model: "claude-3".to_string(),
                function_name: "classify_intent".to_string(),
                prompt: serde_json::json!({"messages": [{"role": "user", "content": "test"}]}),
                metadata: serde_json::json!({
                    "agent_id": agent_id.as_str(),
                    "task_id": task_id.as_str()
                }),
            },
        }))
        .await
        .expect("LlmCallStarted");

    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: ActivityAnchorId::from_counter(10011),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_010_015,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Task {
                    task_id: task_id.clone(),
                },
                client: "anthropic".to_string(),
                model: "claude-3".to_string(),
                function_name: "classify_intent".to_string(),
                prompt: serde_json::json!({"messages": [{"role": "user", "content": "test"}]}),
                metadata: serde_json::json!({
                    "agent_id": agent_id.as_str(),
                    "task_id": task_id.as_str(),
                    "error": "rate limit exceeded - too many requests"
                }),
                usage: LlmUsage::Unknown,
                duration_ms: 500,
                outcome: Outcome::Failure,
                drift: None,
                citations: vec![],
                resolved_citations: vec![],
                prompt_serialized_utf8_bytes: serialized_prompt_utf8_len(
                    &serde_json::json!({"messages": [{"role": "user", "content": "test"}]}),
                ),
            },
        }))
        .await
        .expect("LlmCallCompleted failed");

    // Failed tool call with timeout
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "web_search".to_string(),
            Some("search".to_string()),
            serde_json::json!({"query": "rust programming"}),
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
            Some("search".to_string()),
            serde_json::json!({"query": "rust programming"}),
            serde_json::json!({
                "phase": "send",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
                "error": "timeout connecting to search service"
            }),
            3000,
            Outcome::Failure,
            None,
        ))
        .await
        .expect("tool_call_completed failed");
}

async fn query_failed_calls_ops(store: &dyn SnapshotStore) -> NormalizedOpsResponse {
    let context_id = test_context_id(100);

    // Query LLM calls with FailedOnly
    let request = baml_rt_provenance::store::ProvenanceOpsQueryRequest {
        resource: baml_rt_provenance::store::ProvenanceOpsResource::LlmCalls,
        filters: baml_rt_provenance::store::ProvenanceOpsFilters {
            context_id: Some(context_id),
            ..Default::default()
        },
        outcome: Some(baml_rt_provenance::store::ProvenanceOutcomeSegment::FailedOnly),
        page_size: Some(50),
        ..Default::default()
    };

    let response = store.query_ops(request).await.expect("query_ops");
    normalize_ops_response(&response)
}

snapshot_test!(
    failed_call_with_classification,
    setup_failed_call_with_classification,
    query_failed_calls_ops
);

// ===========================================================================
// Snapshot Test 2: Conversation context with tool calls
// ===========================================================================

async fn setup_conversation_with_tools(store: &dyn SnapshotStore) {
    let context_id = test_context_id(200);
    let task_id = test_task_id("task-conv-tools");
    let agent_id = test_agent_id();

    bootstrap(store, &context_id, &task_id, 20000).await;

    // User message
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: ActivityAnchorId::from_counter(20010),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_020_010,
            data: ProvEventData::MessageReceived {
                id: MessageId::from_external(ExternalId::new("msg-user-1")),
                role: "user".to_string(),
                content: vec!["Calculate 2 + 3 for me".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
                citations: vec![],
            },
        }))
        .await
        .expect("MessageReceived");

    // Successful tool call (phase=execute so it appears in conversation_context)
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "calculator".to_string(),
            Some("add".to_string()),
            serde_json::json!({"a": 2, "b": 3}),
            serde_json::json!({
                "phase": "execute",
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
            serde_json::json!({"a": 2, "b": 3}),
            serde_json::json!({
                "phase": "execute",
                "result": 5,
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            50,
            Outcome::Success,
            None,
        ))
        .await
        .expect("tool_call_completed");

    // Failed tool call (phase=execute so it appears in conversation_context)
    store
        .add_event(ProvEvent::tool_call_started_task(
            context_id.clone(),
            task_id.clone(),
            "divide".to_string(),
            None,
            serde_json::json!({"a": 10, "b": 0}),
            serde_json::json!({
                "phase": "execute",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            None,
        ))
        .await
        .expect("tool_call_started divide");

    store
        .add_event(ProvEvent::tool_call_completed_task(
            context_id.clone(),
            task_id.clone(),
            "divide".to_string(),
            None,
            serde_json::json!({"a": 10, "b": 0}),
            serde_json::json!({
                "phase": "execute",
                "error": "division by zero",
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str()
            }),
            30,
            Outcome::Failure,
            None,
        ))
        .await
        .expect("tool_call_completed divide");

    // Assistant message
    store
        .add_event(ProvEvent::Task(TaskScopedEvent {
            id: ActivityAnchorId::from_counter(20020),
            context_id: context_id.clone(),
            task_id: task_id.clone(),
            timestamp_ms: 1_700_000_020_100,
            data: ProvEventData::MessageSent {
                id: MessageId::from_external(ExternalId::new("msg-agent-1")),
                role: "assistant".to_string(),
                content: vec!["The result of 2 + 3 is 5.".to_string()],
                metadata: None,
                agent_id: agent_id.clone(),
                citations: vec![],
            },
        }))
        .await
        .expect("MessageSent");
}

async fn query_conversation_context(store: &dyn SnapshotStore) -> NormalizedConversationContext {
    let context_id = test_context_id(200);
    let items = store
        .conversation_context(&context_id, None)
        .await
        .expect("conversation_context");
    normalize_conversation_context(&items)
}

snapshot_test!(
    conversation_with_tools,
    setup_conversation_with_tools,
    query_conversation_context
);
