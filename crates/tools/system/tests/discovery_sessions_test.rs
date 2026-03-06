//! Tests for system discovery sessions: discover_agents and discover_tools via SystemBundle.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentLister,
    BusStream, ContextId, Outcome, Result,
    ids::{AgentId, ExternalId, MessageId, UuidId},
};
use baml_rt_provenance::{GraphqliteStoreBuilder, LlmUsage, ProvEvent, ProvenanceWriter};
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
        V::Array(values) => V::Array(values.into_iter().map(redact_runtime_fields).collect()),
        V::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if k == "event_id" {
                    out.insert(k, V::String("[prov_event_id]".to_string()));
                } else if k == "timestamp_ms" {
                    out.insert(k, V::String("[timestamp_ms]".to_string()));
                } else {
                    out.insert(k, redact_runtime_fields(v));
                }
            }
            V::Object(out)
        }
        other => other,
    }
}

fn snapshot_safe_tool_output(output: serde_json::Value) -> serde_json::Value {
    let mut out = output;
    if let Some(payload_json) = out.get("payloadJson").and_then(|v| v.as_str())
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload_json)
    {
        let redacted = redact_runtime_fields(parsed);
        if let Some(obj) = out.as_object_mut() {
            obj.insert("payload".to_string(), redacted);
            obj.remove("payloadJson");
        }
    }
    redact_runtime_fields(out)
}

async fn seeded_store_for_context(
    context_id: &ContextId,
    caller_agent: &AgentId,
    other_agent: &AgentId,
) -> Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
    let store = GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("build store");
    let msg_caller = MessageId::from_external(ExternalId::new("tool-msg-caller".to_string()));
    let msg_other = MessageId::from_external(ExternalId::new("tool-msg-other".to_string()));
    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_caller.clone(),
            "ROLE_USER".to_string(),
            vec!["caller path".to_string()],
            None,
            caller_agent.clone(),
            1,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::llm_call_completed_global(
            context_id.clone(),
            msg_caller.clone(),
            "openai".to_string(),
            "gpt-4o-mini".to_string(),
            "CallerPrompt".to_string(),
            json!({"input":"caller"}),
            call_metadata(caller_agent, &msg_caller, None),
            LlmUsage::Known {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
            },
            100,
            Outcome::Success,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::tool_call_completed_global(
            context_id.clone(),
            msg_caller.clone(),
            "support/calculate".to_string(),
            Some("CalcPrompt".to_string()),
            json!({"expression":"1+1"}),
            call_metadata(caller_agent, &msg_caller, Some("timeout")),
            500,
            Outcome::Failure,
            None,
        ))
        .await
        .unwrap();

    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_other.clone(),
            "ROLE_USER".to_string(),
            vec!["other path".to_string()],
            None,
            other_agent.clone(),
            2,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::llm_call_completed_global(
            context_id.clone(),
            msg_other.clone(),
            "anthropic".to_string(),
            "claude-3-7-sonnet".to_string(),
            "OtherPrompt".to_string(),
            json!({"input":"other"}),
            call_metadata(other_agent, &msg_other, None),
            LlmUsage::Known {
                prompt_tokens: 20,
                completion_tokens: 5,
                total_tokens: 25,
            },
            300,
            Outcome::Success,
        ))
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
    let card = AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        agent_package: pkg.to_string(),
        agent_instance_id: "default".to_string(),
        tools: vec!["system/internal_a2a".to_string()],
        description: description.map(str::to_string),
        capabilities: vec!["a2a".to_string()],
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

    let step = registry.session_next(&session_id).await.unwrap();
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

    let step2 = registry.session_next(&session_id).await.unwrap();
    match &step2 {
        ToolStep::Done { output: None } => {}
        _ => panic!("expected Done(None), got {:?}", step2),
    }
}

#[tokio::test]
async fn discover_tools_session_returns_search_results() {
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

    let step = registry.session_next(&session_id).await.unwrap();
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
                "pageSize": 20
            }),
        )
        .await
        .unwrap();

    let step = registry.session_next(&session_id).await.unwrap();
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
    let payload = output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .expect("payloadJson string");
    insta::assert_json_snapshot!(
        "introspection_tool_request_scope",
        redact_runtime_fields(payload)
    );
}

#[tokio::test]
async fn extrospection_session_snapshots_cross_scope_request() {
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
                "outcome": "failed_only",
                "pageSize": 5
            }),
        )
        .await
        .unwrap();

    let step = registry.session_next(&session_id).await.unwrap();
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
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .expect("payloadJson string");
    insta::assert_json_snapshot!(
        "extrospection_tool_request_scope",
        redact_runtime_fields(payload)
    );
}

#[tokio::test]
async fn introspection_session_pagination_snapshots() {
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
    let first = registry.session_next(&session_id).await.unwrap();

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
    let second = registry.session_next(&session_id).await.unwrap();

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
    let filtered = registry.session_next(&session_id).await.unwrap();

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
    let drilldown = registry.session_next(&session_id).await.unwrap();

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
