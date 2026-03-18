//! Tests for system discovery sessions: discover_agents and discover_tools via SystemBundle.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentLister,
    BusStream, ContextId, EventSchemaVersion, EventSourceKind, EventSubscription, Outcome, Result,
    ids::{AgentId, EventId, ExternalId, MessageId, UuidId},
};
use baml_rt_provenance::{
    CallScope, GlobalEvent, GraphqliteStoreBuilder, LlmUsage, ProvEvent, ProvEventData,
    ProvenanceWriter,
};
use baml_rt_tools::{ToolRegistry, ToolStep};
use baml_tools_calculator::CalculatorTool;
use baml_tools_system::SystemBundle;
use futures_util::stream;
use serde_json::json;

struct MockA2aHandler;

#[async_trait]
impl A2aRequestHandler for MockA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        Ok(Box::pin(stream::empty::<A2aStreamChunk>()))
    }
}

struct MockAgentList {
    entries: Vec<AgentDiscoveryEntry>,
}

fn suite_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn schema_version(value: &str) -> EventSchemaVersion {
    EventSchemaVersion::parse(value).expect("valid schema version")
}

fn source_kind(value: &str) -> EventSourceKind {
    EventSourceKind::parse(value).expect("valid source kind")
}

fn call_metadata(
    agent_id: &AgentId,
    message_id: &MessageId,
    error: Option<&str>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "agent_id".to_string(),
        serde_json::Value::String(agent_id.as_str().to_string()),
    );
    map.insert(
        "message_id".to_string(),
        serde_json::Value::String(message_id.as_str().to_string()),
    );
    if let Some(error) = error {
        map.insert(
            "error".to_string(),
            serde_json::Value::String(error.to_string()),
        );
    }
    serde_json::Value::Object(map)
}

fn redact_runtime_fields(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value as V;
    match value {
        V::String(s) if s.starts_with("prov-") => V::String("[prov_event_id]".to_string()),
        V::String(s) if s.starts_with("prov:v1:payload:") => {
            V::String("[prov_payload_ref]".to_string())
        }
        V::String(s) if s.starts_with("prov:v1:activity:") => {
            V::String("[prov_activity_ref]".to_string())
        }
        V::Array(values) => V::Array(values.into_iter().map(redact_runtime_fields).collect()),
        V::Object(map) => {
            // Sort keys explicitly via BTreeMap so output is alphabetical regardless of
            // whether serde_json uses IndexMap (preserve_order feature, activated by BAML
            // canary transitive dep on Linux CI) or BTreeMap (macOS without that feature).
            let mut sorted: std::collections::BTreeMap<String, V> =
                std::collections::BTreeMap::new();
            for (k, v) in map {
                let val = if k == "event_id" {
                    V::String("[prov_event_id]".to_string())
                } else if k == "timestamp_ms" {
                    V::String("[timestamp_ms]".to_string())
                } else {
                    redact_runtime_fields(v)
                };
                sorted.insert(k, val);
            }
            V::Object(sorted.into_iter().collect())
        }
        // Normalize floats for cross-platform stability:
        // - Integer-valued floats (0.0 → 0, 220.0 → 220): SQLite/graphqlite may return the
        //   same logical integer as float on one platform and integer on another.
        // - Non-integer floats (percentiles, ratios): round to 2 decimal places to eliminate
        //   ARM64 vs x86_64 floating-point rounding noise from graphqlite C aggregates
        //   (e.g. p95 computed as 491.50000000000006 on Linux vs 491.5 on macOS).
        V::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(0.0);
            if !f.is_finite() {
                V::Number(n)
            } else if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                V::Number(serde_json::Number::from(f as i64))
            } else {
                // Round to 2 decimal places to absorb platform FP noise.
                let rounded = (f * 100.0).round() / 100.0;
                V::Number(serde_json::Number::from_f64(rounded).unwrap_or(n))
            }
        }
        other => other,
    }
}

/// Extract only the query-result payload from a tool output, discarding infrastructure
/// metadata (`budgetExhausted`, `retrievalBudget`, `historyContext`, `done`) that varies
/// across platforms (macOS vs Linux) even with deterministic in-memory stores.
/// Snapshots should capture what the agent *sees* (the data), not bookkeeping fields.
fn snapshot_safe_tool_output(output: serde_json::Value) -> serde_json::Value {
    // Prefer payloadJson (the serialized query result) — always present for normal queries.
    if let Some(payload_json) = output.get("payloadJson").and_then(|v| v.as_str())
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload_json)
    {
        return redact_runtime_fields(parsed);
    }
    // Fallback: if already unwrapped (e.g. Linux CI returns payload directly), use as-is.
    if let Some(payload) = output.get("payload") {
        return redact_runtime_fields(payload.clone());
    }
    redact_runtime_fields(output)
}

async fn seeded_store_for_context(
    context_id: &ContextId,
    caller_agent: &AgentId,
    other_agent: &AgentId,
) -> Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
    // Use isolated in-memory stores: avoids WAL/file-IO platform differences (macOS vs Linux)
    // that caused non-deterministic snapshot content across CI environments.
    //
    // ALL events use hardcoded monotonic timestamps (1, 2, 3, …) so Cypher ORDER BY
    // timestamp_ms produces deterministic results regardless of platform clock resolution.
    // Events created with now_millis() could collide on fast machines, causing non-deterministic
    // sort order and snapshot mismatches between macOS (ARM64) and Linux CI (x86_64).
    let store = GraphqliteStoreBuilder::in_memory_isolated()
        .build()
        .expect("build isolated in-memory store");
    let msg_caller = MessageId::from_external(ExternalId::new("tool-msg-caller".to_string()));
    let msg_other = MessageId::from_external(ExternalId::new("tool-msg-other".to_string()));
    let msg_linked = MessageId::from_external(ExternalId::new("tool-msg-linked".to_string()));

    // ts=100: caller message received
    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_caller.clone(),
            "ROLE_USER".to_string(),
            vec!["caller path".to_string()],
            None,
            caller_agent.clone(),
            100,
        ))
        .await
        .unwrap();

    // ts=200: caller LLM call (success)
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(10),
            context_id: context_id.clone(),
            timestamp_ms: 200,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_caller.clone(),
                },
                client: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                function_name: "CallerPrompt".to_string(),
                prompt: json!({"input":"caller"}),
                metadata: call_metadata(caller_agent, &msg_caller, None),
                usage: LlmUsage::Known {
                    prompt_tokens: 5,
                    completion_tokens: 3,
                    total_tokens: 8,
                    cached_input_tokens: None,
                },
                duration_ms: 100,
                outcome: Outcome::Success,
                drift: None,
            },
        }))
        .await
        .unwrap();

    // ts=300: caller LLM call (failure — linked to PromptRejected below)
    let caller_failed_llm_event_id = EventId::from_counter(900);
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: caller_failed_llm_event_id.clone(),
            context_id: context_id.clone(),
            timestamp_ms: 300,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_caller.clone(),
                },
                client: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                function_name: "CallerPrompt".to_string(),
                prompt: json!({"input":"caller-failed"}),
                metadata: call_metadata(caller_agent, &msg_caller, None),
                usage: LlmUsage::Known {
                    prompt_tokens: 7,
                    completion_tokens: 0,
                    total_tokens: 7,
                    cached_input_tokens: None,
                },
                duration_ms: 220,
                outcome: Outcome::Failure,
                drift: None,
            },
        }))
        .await
        .unwrap();

    // ts=400: prompt rejected (linked to failed LLM call above)
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(20),
            context_id: context_id.clone(),
            timestamp_ms: 400,
            data: ProvEventData::PromptRejected {
                scope: CallScope::Message {
                    message_id: msg_caller.clone(),
                },
                llm_call_event_id: caller_failed_llm_event_id,
                reason: "BAML validation failed: missing required field".to_string(),
            },
        }))
        .await
        .unwrap();

    // ts=500: caller message sent
    store
        .add_event(ProvEvent::message_sent_global(
            context_id.clone(),
            msg_caller.clone(),
            "ROLE_AGENT".to_string(),
            vec!["BAML validation failed: missing required field".to_string()],
            None,
            caller_agent.clone(),
            500,
        ))
        .await
        .unwrap();

    // ts=600: tool call (support/calculate, failure)
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(30),
            context_id: context_id.clone(),
            timestamp_ms: 600,
            data: ProvEventData::ToolCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_caller.clone(),
                },
                tool_name: "support/calculate".to_string(),
                function_name: Some("CalcPrompt".to_string()),
                args: json!({"expression":"1+1"}),
                metadata: call_metadata(caller_agent, &msg_caller, Some("timeout")),
                duration_ms: 500,
                outcome: Outcome::Failure,
                delegation_target: None,
            },
        }))
        .await
        .unwrap();

    // ts=700: linked message received
    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_linked.clone(),
            "ROLE_USER".to_string(),
            vec!["linked evidence path".to_string()],
            None,
            caller_agent.clone(),
            700,
        ))
        .await
        .unwrap();

    // ts=800: tool call (support/delegate, failure)
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(40),
            context_id: context_id.clone(),
            timestamp_ms: 800,
            data: ProvEventData::ToolCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_linked.clone(),
                },
                tool_name: "support/delegate".to_string(),
                function_name: Some("DelegatePrompt".to_string()),
                args: json!({"objective":"linked emitted message evidence"}),
                metadata: call_metadata(caller_agent, &msg_linked, None),
                duration_ms: 330,
                outcome: Outcome::Failure,
                delegation_target: None,
            },
        }))
        .await
        .unwrap();

    // ts=900: linked message sent
    store
        .add_event(ProvEvent::message_sent_global(
            context_id.clone(),
            msg_linked,
            "ROLE_AGENT".to_string(),
            vec!["authentication failed: 401 unauthorized invalid api key".to_string()],
            None,
            caller_agent.clone(),
            900,
        ))
        .await
        .unwrap();

    // ts=1000: other agent message received
    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_other.clone(),
            "ROLE_USER".to_string(),
            vec!["other path".to_string()],
            None,
            other_agent.clone(),
            1000,
        ))
        .await
        .unwrap();

    // ts=1100: other agent LLM call (success)
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: EventId::from_counter(50),
            context_id: context_id.clone(),
            timestamp_ms: 1100,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_other.clone(),
                },
                client: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                function_name: "OtherPrompt".to_string(),
                prompt: json!({"input":"other"}),
                metadata: call_metadata(other_agent, &msg_other, None),
                usage: LlmUsage::Known {
                    prompt_tokens: 20,
                    completion_tokens: 5,
                    total_tokens: 25,
                    cached_input_tokens: None,
                },
                duration_ms: 300,
                outcome: Outcome::Success,
                drift: None,
            },
        }))
        .await
        .unwrap();

    store
}

impl MockAgentList {
    fn new(entries: Vec<AgentDiscoveryEntry>) -> Self {
        Self { entries }
    }
}

impl AgentLister for MockAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

fn entry_with_card(
    pkg: &str,
    name: &str,
    version: &str,
    description: Option<&str>,
) -> AgentDiscoveryEntry {
    entry_with_capabilities(pkg, name, version, description, vec!["a2a"])
}

fn entry_with_capabilities(
    pkg: &str,
    name: &str,
    version: &str,
    description: Option<&str>,
    capabilities: Vec<&str>,
) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        agent_package: pkg.to_string(),
        agent_instance_id: "default".to_string(),
        tools: vec!["system/internal_a2a".to_string()],
        baml_functions: vec![],
        description: description.map(str::to_string),
        capabilities: capabilities.into_iter().map(str::to_string).collect(),
        subscriptions: vec![],
    };
    AgentDiscoveryEntry {
        agent_package: pkg.to_string(),
        agent_instance_id: "default".to_string(),
        name: name.to_string(),
        version: version.to_string(),
        agent_card: card,
    }
}

fn entry_with_subscriptions(
    pkg: &str,
    name: &str,
    version: &str,
    description: Option<&str>,
    subscriptions: Vec<EventSubscription>,
) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        agent_package: pkg.to_string(),
        agent_instance_id: "default".to_string(),
        tools: vec!["system/internal_a2a".to_string()],
        baml_functions: vec![],
        description: description.map(str::to_string),
        capabilities: vec!["a2a".to_string()],
        subscriptions,
    };
    AgentDiscoveryEntry {
        agent_package: pkg.to_string(),
        agent_instance_id: "default".to_string(),
        name: name.to_string(),
        version: version.to_string(),
        agent_card: card,
    }
}

#[tokio::test]
async fn discover_agents_session_returns_paged_cards() {
    let _suite_guard = suite_lock().lock().await;
    let entries = vec![
        entry_with_card("pkg-a", "Agent A", "0.1.0", Some("Does A")),
        entry_with_card("pkg-b", "Agent B", "0.2.0", None),
    ];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 1),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(&session_id, json!({ "limit": 10 }))
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let agents = output.get("agents").and_then(|a| a.as_array()).unwrap();
            assert_eq!(agents.len(), 2);
            assert_eq!(
                agents[0].get("name").and_then(|v| v.as_str()),
                Some("Agent A")
            );
            assert_eq!(
                agents[1].get("name").and_then(|v| v.as_str()),
                Some("Agent B")
            );
            assert_eq!(output.get("done").and_then(|v| v.as_bool()), Some(true));
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }

    let step2 = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step2 {
        ToolStep::Done {
            output: Some(output),
        } => {
            assert_eq!(output.get("done").and_then(|v| v.as_bool()), Some(true));
        }
        _ => panic!("expected Done(Some(output)), got {:?}", step2),
    }
}

#[tokio::test]
async fn discover_agents_null_send_filter_defaults_to_all_agents() {
    let _suite_guard = suite_lock().lock().await;
    let entries = vec![
        entry_with_card("pkg-a", "Agent A", "0.1.0", Some("Does A")),
        entry_with_card("pkg-b", "Agent B", "0.2.0", Some("Does B")),
    ];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000011").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 11),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "query": null,
                "limit": null,
                "offset": null
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let agents = output.get("agents").and_then(|a| a.as_array()).unwrap();
            assert_eq!(agents.len(), 2);
            assert_eq!(
                agents[0].get("name").and_then(|v| v.as_str()),
                Some("Agent A")
            );
            assert_eq!(
                agents[1].get("name").and_then(|v| v.as_str()),
                Some("Agent B")
            );
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn discover_agents_session_filters_by_required_capabilities() {
    let entries = vec![
        entry_with_capabilities(
            "clickup-agent",
            "ClickUp Agent",
            "1.0.0",
            Some("Works with ClickUp tasks"),
            vec!["clickup:get-task", "clickup:create-task", "a2a"],
        ),
        entry_with_capabilities(
            "notion-agent",
            "Notion Agent",
            "1.0.0",
            Some("Works with Notion pages"),
            vec!["notion:read-page", "a2a"],
        ),
    ];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000011").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 11),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "requiredCapabilities": ["clickup:get-task", "clickup:create-task"],
                "limit": 10
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let agents = output.get("agents").and_then(|a| a.as_array()).unwrap();
            assert_eq!(agents.len(), 1);
            assert_eq!(
                agents[0].get("agentPackage").and_then(|v| v.as_str()),
                Some("clickup-agent")
            );
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn discover_agents_capability_filter_is_not_overridden_by_query_fallback() {
    let entries = vec![entry_with_capabilities(
        "clickup-agent",
        "ClickUp Agent",
        "1.0.0",
        Some("Works with ClickUp tasks"),
        vec!["clickup:get-task", "a2a"],
    )];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000012").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 12),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "query": "no-match",
                "requiredCapabilities": ["clickup:create-task"],
                "limit": 10
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let agents = output.get("agents").and_then(|a| a.as_array()).unwrap();
            assert!(
                agents.is_empty(),
                "capability filtering must remain authoritative when query fallback would otherwise return all agents"
            );
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn discover_agents_capability_filter_is_case_insensitive() {
    let entries = vec![
        entry_with_capabilities(
            "clickup-agent",
            "ClickUp Agent",
            "1.0.0",
            Some("Works with ClickUp tasks"),
            vec!["ClickUp:Get-Task", "A2A"],
        ),
        entry_with_capabilities(
            "notion-agent",
            "Notion Agent",
            "1.0.0",
            Some("Works with Notion pages"),
            vec!["notion:read-page"],
        ),
    ];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000013").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 13),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "requiredCapabilities": ["clickup:get-task"],
                "limit": 10
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let agents = output.get("agents").and_then(|a| a.as_array()).unwrap();
            assert_eq!(agents.len(), 1);
            assert_eq!(
                agents[0].get("agentPackage").and_then(|v| v.as_str()),
                Some("clickup-agent")
            );
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn discover_agents_session_returns_declared_subscriptions() {
    let entries = vec![entry_with_subscriptions(
        "workflow-intake-agent",
        "Workflow Intake",
        "1.0.0",
        Some("Consumes task-daemon events"),
        vec![EventSubscription {
            schema_versions: vec![schema_version("task-daemon.interpretation.v1")],
            source_kinds: vec![source_kind("slack"), source_kind("clickup")],
            ..EventSubscription::default()
        }],
    )];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-00000000001a").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 26),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(&session_id, json!({ "limit": 10 }))
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let subscriptions = output
                .pointer("/agents/0/subscriptions")
                .and_then(|value| value.as_array())
                .expect("subscriptions should be present");
            assert_eq!(subscriptions.len(), 1);
            assert_eq!(
                subscriptions[0]
                    .get("schemaVersions")
                    .or_else(|| subscriptions[0].get("schema_versions"))
                    .and_then(|value| value.as_array())
                    .map(|items| items.len()),
                Some(1)
            );
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn discover_agents_session_filters_by_event_subscription() {
    let entries = vec![
        entry_with_subscriptions(
            "workflow-intake-agent",
            "Workflow Intake",
            "1.0.0",
            Some("Consumes task-daemon Slack events"),
            vec![EventSubscription {
                schema_versions: vec![schema_version("task-daemon.interpretation.v1")],
                source_kinds: vec![source_kind("slack")],
                ..EventSubscription::default()
            }],
        ),
        entry_with_subscriptions(
            "clickup-reconciler",
            "ClickUp Reconciler",
            "1.0.0",
            Some("Consumes task-daemon ClickUp events"),
            vec![EventSubscription {
                schema_versions: vec![schema_version("task-daemon.interpretation.v1")],
                source_kinds: vec![source_kind("clickup")],
                ..EventSubscription::default()
            }],
        ),
    ];
    let agent_list = Arc::new(MockAgentList::new(entries));
    let registry = Arc::new(ToolRegistry::new());
    let a2a_handler = Arc::new(MockA2aHandler);
    registry
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-00000000001b").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_agents",
            json!({}),
            &ContextId::new(1, 27),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "requiredSchemaVersions": ["task-daemon.interpretation.v1"],
                "requiredSourceKinds": ["clickup"],
                "limit": 10
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let agents = output
                .get("agents")
                .and_then(|value| value.as_array())
                .unwrap();
            assert_eq!(agents.len(), 1);
            assert_eq!(
                agents[0]
                    .get("agentPackage")
                    .or_else(|| agents[0].get("agent_package"))
                    .and_then(|value| value.as_str()),
                Some("clickup-reconciler")
            );
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn discover_tools_session_returns_search_results() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    registry.register(CalculatorTool).unwrap();
    registry
        .register_bundle(SystemBundle::new(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
        ))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000002").unwrap());
    let session_id = registry
        .open_session(
            "system/discover_tools",
            json!({}),
            &ContextId::new(1, 2),
            &agent_id,
        )
        .await
        .unwrap();

    registry
        .session_send(&session_id, json!({ "query": "calc", "limit": 10 }))
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    match &step {
        ToolStep::Done {
            output: Some(output),
        } => {
            let tools = output.get("tools").and_then(|t| t.as_array()).unwrap();
            assert!(
                !tools.is_empty(),
                "search 'calc' should match support/calculate"
            );
            let names: Vec<&str> = tools
                .iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
                .collect();
            assert!(
                names.contains(&"support/calculate"),
                "expected support/calculate in {:?}",
                names
            );
            assert_eq!(output.get("done").and_then(|v| v.as_bool()), Some(true));
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}

#[tokio::test]
async fn introspection_session_snapshots_compact_result_and_agent_scope() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 9);
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(
                &context,
                &AgentId::from_uuid(
                    UuidId::parse_str("00000000-0000-0000-0000-000000000123").unwrap(),
                ),
                &AgentId::from_uuid(
                    UuidId::parse_str("00000000-0000-0000-0000-000000000999").unwrap(),
                ),
            )
            .await,
        ))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000123").unwrap());
    let requested_agent =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000999").unwrap());
    let session_id = registry
        .open_session("system/introspection", json!({}), &context, &agent_id)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "agentId": requested_agent.as_str(),
                "groupBy": ["agent_id"],
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "pageSize": 20
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let output = match step {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected Done(Some(output)), got {:?}", other),
    };
    insta::assert_json_snapshot!(
        "introspection_tool_output",
        snapshot_safe_tool_output(output.clone())
    );
    // Extract the payload from payloadJson or from "payload" key (platform-independent path).
    let payload = output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .or_else(|| output.get("payload").cloned())
        .expect("payloadJson or payload field must be present");
    insta::assert_json_snapshot!(
        "introspection_tool_request_scope",
        redact_runtime_fields(payload)
    );
}

#[tokio::test]
async fn extrospection_session_snapshots_cross_scope_request() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 10);
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(
                &context,
                &AgentId::from_uuid(
                    UuidId::parse_str("00000000-0000-0000-0000-000000000124").unwrap(),
                ),
                &AgentId::from_uuid(
                    UuidId::parse_str("00000000-0000-0000-0000-000000000999").unwrap(),
                ),
            )
            .await,
        ))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000124").unwrap());
    let cross_context = ContextId::new(123, 4).to_string();
    let cross_agent =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000999").unwrap());
    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &agent_id)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "tool_calls",
                "contextId": cross_context,
                "agentId": cross_agent.as_str(),
                "groupBy": ["agent_id", "tool_name"],
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "outcome": "failed_only",
                "pageSize": 5
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let output = match step {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected Done(Some(output)), got {:?}", other),
    };
    insta::assert_json_snapshot!(
        "extrospection_tool_output",
        snapshot_safe_tool_output(output.clone())
    );
    let payload = output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .or_else(|| output.get("payload").cloned())
        .expect("payloadJson or payload field must be present");
    insta::assert_json_snapshot!(
        "extrospection_tool_request_scope",
        redact_runtime_fields(payload)
    );
}

#[tokio::test]
async fn extrospection_session_defaults_to_invocation_scope_without_overrides() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 16);
    let caller =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000712").unwrap());
    let other =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000713").unwrap());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(&context, &caller, &other).await,
        ))
        .unwrap();

    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &caller)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "pageSize": 50
            }),
        )
        .await
        .unwrap();

    let step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let output = match step {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected Done(Some(output)), got {:?}", other),
    };
    let payload = output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .or_else(|| output.get("payload").cloned())
        .expect("payloadJson or payload field must be present");
    let rows = payload
        .get("rows")
        .and_then(|v| v.as_array())
        .expect("rows array");
    assert!(!rows.is_empty(), "expected scoped extrospection rows");
    for row in rows {
        assert_eq!(
            row.get("context_id").and_then(|v| v.as_str()),
            Some(context.as_str())
        );
        assert_eq!(
            row.get("agent_id").and_then(|v| v.as_str()),
            Some(caller.as_str())
        );
    }
}

#[tokio::test]
async fn extrospection_session_retrieve_ref_returns_typed_archive_record() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 17);
    let caller =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000714").unwrap());
    let other =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000715").unwrap());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(&context, &caller, &other).await,
        ))
        .unwrap();

    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &caller)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "agentId": caller.as_str(),
                "pageSize": 1
            }),
        )
        .await
        .unwrap();

    let first = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let first_output = match first {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected first Done(Some(output)), got {:?}", other),
    };
    let first_payload = first_output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .or_else(|| first_output.get("payload").cloned())
        .expect("payloadJson or payload field must be present");
    let rows = first_payload
        .get("rows")
        .and_then(|v| v.as_array())
        .expect("rows array");
    let retrieve_ref = rows
        .iter()
        .find_map(|row| row.get("llm_call_ref").and_then(|v| v.as_str()))
        .map(str::to_string)
        .expect("llm_call_ref in rows");

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "read": {
                    "refId": retrieve_ref
                }
            }),
        )
        .await
        .unwrap();

    let second = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let second_output = match second {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected second Done(Some(output)), got {:?}", other),
    };
    assert!(
        second_output.get("payloadJson").is_none(),
        "read envelope path must return typed readResult without payloadJson wrapper"
    );
    let read_result = second_output
        .get("readResult")
        .and_then(|v| v.as_object())
        .expect("readResult object");
    assert_eq!(
        read_result.get("projection").and_then(|v| v.as_str()),
        Some("summary"),
        "default projection should be summary when omitted"
    );
    let refs = read_result
        .get("refs")
        .and_then(|v| v.as_array())
        .expect("readResult.refs array");
    assert_eq!(
        refs.first().and_then(|v| v.as_str()),
        Some(retrieve_ref.as_str()),
        "refs must prepend the traversal ref"
    );
    let archive_summary = read_result
        .get("archiveSummary")
        .and_then(|v| v.as_object())
        .expect("readResult.archiveSummary object");
    assert!(
        archive_summary
            .get("payloadCount")
            .and_then(|v| v.as_u64())
            .is_some(),
        "archiveSummary.payloadCount should be present"
    );
    let archive_record = read_result.get("archiveRecord").and_then(|v| v.as_object());
    assert!(
        archive_record.is_none(),
        "summary projection should not include archiveRecord detail payloads"
    );

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "read": {
                    "refId": retrieve_ref,
                    "projection": "identity"
                }
            }),
        )
        .await
        .unwrap();
    let identity_step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let identity_output = match identity_step {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected identity Done(Some(output)), got {:?}", other),
    };
    let identity_result = identity_output
        .get("readResult")
        .and_then(|v| v.as_object())
        .expect("identity readResult object");
    assert_eq!(
        identity_result.get("projection").and_then(|v| v.as_str()),
        Some("identity")
    );
    assert!(
        identity_result.get("archiveSummary").is_none(),
        "identity projection should omit archive summary"
    );
    assert!(
        identity_result.get("archiveRecord").is_none(),
        "identity projection should omit archive record"
    );

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "read": {
                    "refId": retrieve_ref,
                    "projection": "detail"
                }
            }),
        )
        .await
        .unwrap();
    let detail_step = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let detail_output = match detail_step {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected detail Done(Some(output)), got {:?}", other),
    };
    let detail_result = detail_output
        .get("readResult")
        .and_then(|v| v.as_object())
        .expect("detail readResult object");
    assert_eq!(
        detail_result.get("projection").and_then(|v| v.as_str()),
        Some("detail")
    );
    let detail_record = detail_result
        .get("archiveRecord")
        .and_then(|v| v.as_object())
        .expect("detail archiveRecord");
    let payloads = detail_record
        .get("payloads")
        .and_then(|v| v.as_array())
        .expect("detail archiveRecord.payloads array");
    assert!(
        !payloads.is_empty(),
        "detail projection should include archive payload entries"
    );
    let source = payloads[0].get("source").and_then(|v| v.as_str());
    assert!(
        matches!(
            source,
            Some("llm_call") | Some("llm_result") | Some("tool_call") | Some("tool_result")
        ),
        "unexpected archive payload source: {:?}",
        source
    );
}

#[tokio::test]
async fn extrospection_retrieve_ref_enforces_hard_budget_caps() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 19);
    let caller =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000716").unwrap());
    let other =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000717").unwrap());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(&context, &caller, &other).await,
        ))
        .unwrap();

    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &caller)
        .await
        .unwrap();
    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "agentId": caller.as_str(),
                "pageSize": 1
            }),
        )
        .await
        .unwrap();
    let initial = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let initial_output = match initial {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected Done(Some(output)), got {:?}", other),
    };
    let payload = initial_output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .or_else(|| initial_output.get("payload").cloned())
        .expect("payloadJson or payload field must be present");
    let rows = payload
        .get("rows")
        .and_then(|v| v.as_array())
        .expect("rows array");
    let retrieve_ref = rows
        .iter()
        .find_map(|row| row.get("llm_call_ref").and_then(|v| v.as_str()))
        .map(str::to_string)
        .expect("llm_call_ref in rows");

    let mut exhausted = false;
    for _ in 0..10 {
        registry
            .session_send(
                &session_id,
                json!({
                    "resource": "llm_calls",
                "read": {
                    "refId": retrieve_ref
                }
                }),
            )
            .await
            .unwrap();
        let step = registry
            .session_read(&session_id, serde_json::Value::Null)
            .await
            .unwrap();
        let output = match step {
            ToolStep::Done {
                output: Some(output),
            } => output,
            other => panic!("expected Done(Some(output)), got {:?}", other),
        };
        exhausted = output
            .get("budgetExhausted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if exhausted {
            break;
        }
    }
    assert!(
        exhausted,
        "read envelope loop should eventually hit hard budget cap"
    );
}

#[tokio::test]
async fn introspection_session_pagination_snapshots() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 11);
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(
                &context,
                &AgentId::from_uuid(
                    UuidId::parse_str("00000000-0000-0000-0000-000000000555").unwrap(),
                ),
                &AgentId::from_uuid(
                    UuidId::parse_str("00000000-0000-0000-0000-000000000556").unwrap(),
                ),
            )
            .await,
        ))
        .unwrap();

    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000555").unwrap());
    let session_id = registry
        .open_session("system/introspection", json!({}), &context, &agent_id)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "groupBy": ["agent_id", "model"],
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "pageSize": 1
            }),
        )
        .await
        .unwrap();
    let first = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "groupBy": ["agent_id", "model"],
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "pageSize": 1,
                "cursor": "1"
            }),
        )
        .await
        .unwrap();
    let second = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    let first_output = match first {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected first Done(Some(output)), got {:?}", other),
    };
    let second_output = match second {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected second Done(Some(output)), got {:?}", other),
    };

    insta::assert_json_snapshot!(
        "introspection_tool_pagination",
        redact_runtime_fields(serde_json::json!({
            "page1": snapshot_safe_tool_output(first_output),
            "page2": snapshot_safe_tool_output(second_output)
        }))
    );
}

#[tokio::test]
async fn extrospection_session_filter_sort_and_drilldown_snapshots() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 12);
    let caller =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000700").unwrap());
    let other =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000701").unwrap());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(&context, &caller, &other).await,
        ))
        .unwrap();

    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &caller)
        .await
        .unwrap();
    let context_id = context.to_string();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "tool_calls",
                "contextId": context_id.clone(),
                "outcome": "failed_only",
                "groupBy": ["agent_id", "tool_name"],
                "sortBy": "duration_ms",
                "sortDir": "desc",
                "pageSize": 5
            }),
        )
        .await
        .unwrap();
    let filtered = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "tool_calls",
                "contextId": context_id,
                "agentId": caller.as_str(),
                "toolName": "support/calculate",
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "pageSize": 1
            }),
        )
        .await
        .unwrap();
    let drilldown = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    let filtered_output = match filtered {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected filtered Done(Some(output)), got {:?}", other),
    };
    let drilldown_output = match drilldown {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected drilldown Done(Some(output)), got {:?}", other),
    };

    insta::assert_json_snapshot!(
        "extrospection_tool_filter_sort_drilldown",
        redact_runtime_fields(serde_json::json!({
            "filtered": snapshot_safe_tool_output(filtered_output),
            "drilldown": snapshot_safe_tool_output(drilldown_output)
        }))
    );
}

#[tokio::test]
async fn extrospection_session_auto_drilldown_from_hotspot_snapshot() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 14);
    let caller =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000710").unwrap());
    let other =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000711").unwrap());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(&context, &caller, &other).await,
        ))
        .unwrap();

    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &caller)
        .await
        .unwrap();
    let context_id = context.to_string();

    // Pass 1: aggregate sweep.
    registry
        .session_send(
            &session_id,
            json!({
                "resource": "tool_calls",
                "contextId": context_id.clone(),
                "outcome": "failed_only",
                "groupBy": ["agent_id", "tool_name", "baml_prompt"],
                "sortBy": "duration_ms",
                "sortDir": "desc",
                "pageSize": 5
            }),
        )
        .await
        .unwrap();
    let pass1 = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let pass1_output = match pass1 {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected pass1 Done(Some(output)), got {:?}", other),
    };

    // Derive pass-2 drilldown from pass-1 hotspot/rows, like the agent FSM does.
    let pass1_payload = pass1_output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or_else(|| json!({}));

    let mut derived_agent_id: Option<String> = None;
    let mut derived_tool_name: Option<String> = None;
    let mut derived_prompt: Option<String> = None;

    if let Some(group0) = pass1_payload
        .get("hotspotGroups")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        && let Some(values) = group0.get("groupValues").and_then(|v| v.as_array())
    {
        derived_agent_id = values
            .first()
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        derived_tool_name = values
            .get(1)
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        derived_prompt = values
            .get(2)
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
    }

    if let Some(row0) = pass1_payload
        .get("rows")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
    {
        if derived_agent_id.is_none() {
            derived_agent_id = row0
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
        if derived_tool_name.is_none() {
            derived_tool_name = row0
                .get("tool_name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
        if derived_prompt.is_none() {
            derived_prompt = row0
                .get("baml_prompt")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }
    }

    // Pass 2: focused drilldown in the same session.
    registry
        .session_send(
            &session_id,
            json!({
                "resource": "tool_calls",
                "contextId": context_id,
                "agentId": derived_agent_id,
                "toolName": derived_tool_name,
                "bamlPrompt": derived_prompt,
                "outcome": "failed_only",
                "groupBy": ["agent_id", "tool_name", "baml_prompt"],
                "sortBy": "duration_ms",
                "sortDir": "desc",
                "pageSize": 3
            }),
        )
        .await
        .unwrap();
    let pass2 = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();
    let pass2_output = match pass2 {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected pass2 Done(Some(output)), got {:?}", other),
    };

    insta::assert_json_snapshot!(
        "extrospection_tool_auto_drilldown_from_hotspot",
        redact_runtime_fields(serde_json::json!({
            "pass1": snapshot_safe_tool_output(pass1_output),
            "derived_filters": {
                "agentId": derived_agent_id,
                "toolName": derived_tool_name,
                "bamlPrompt": derived_prompt
            },
            "pass2": snapshot_safe_tool_output(pass2_output)
        }))
    );
}

#[tokio::test]
async fn extrospection_session_failure_evidence_linked_modes_snapshots() {
    let _suite_guard = suite_lock().lock().await;
    let registry = Arc::new(ToolRegistry::new());
    let context = ContextId::new(9, 13);
    let caller =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000702").unwrap());
    let other =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000703").unwrap());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(MockAgentList::new(vec![])),
            registry.clone(),
            Arc::new(MockA2aHandler),
            seeded_store_for_context(&context, &caller, &other).await,
        ))
        .unwrap();

    let session_id = registry
        .open_session("system/extrospection", json!({}), &context, &caller)
        .await
        .unwrap();
    let context_id = context.to_string();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "llm_calls",
                "contextId": context_id.clone(),
                "outcome": "failed_only",
                "provider": "openai",
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "pageSize": 5
            }),
        )
        .await
        .unwrap();
    let llm = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    registry
        .session_send(
            &session_id,
            json!({
                "resource": "tool_calls",
                "contextId": context_id,
                "toolName": "support/delegate",
                "outcome": "failed_only",
                "sortBy": "timestamp_ms",
                "sortDir": "asc",
                "pageSize": 5
            }),
        )
        .await
        .unwrap();
    let tool = registry
        .session_read(&session_id, serde_json::Value::Null)
        .await
        .unwrap();

    let llm_output = match llm {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected llm Done(Some(output)), got {:?}", other),
    };
    let tool_output = match tool {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected tool Done(Some(output)), got {:?}", other),
    };

    insta::assert_json_snapshot!(
        "extrospection_tool_failure_evidence_linked_modes",
        redact_runtime_fields(serde_json::json!({
            "llm_linked_prompt_rejected": snapshot_safe_tool_output(llm_output),
            "tool_linked_emitted_message": snapshot_safe_tool_output(tool_output)
        }))
    );
}
