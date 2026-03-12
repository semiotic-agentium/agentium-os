//! Tests for system discovery sessions: discover_agents and discover_tools via SystemBundle.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentLister,
    BusStream, ContextId, Outcome, Result,
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

/// Stabilize ops payload for deterministic snapshots across CI (HashMap iteration, float repr).
fn stabilize_ops_payload(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value as V;
    match value {
        V::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap();
            V::Number(serde_json::Number::from_f64((f * 100.0).round() / 100.0).unwrap_or(n))
        }
        V::Array(values) => V::Array(values.into_iter().map(stabilize_ops_payload).collect()),
        V::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let v = stabilize_ops_payload(v);
                out.insert(k, v);
            }
            // Sort hotspotGroups and rows for deterministic order (HashMap iteration can vary).
            if let Some(V::Array(groups)) = out.get("hotspotGroups") {
                let mut sorted: Vec<_> = groups.clone();
                sorted.sort_by(|a, b| {
                    let ak = a.get("groupKey").and_then(|v| v.as_str()).unwrap_or("");
                    let bk = b.get("groupKey").and_then(|v| v.as_str()).unwrap_or("");
                    ak.cmp(bk)
                });
                out.insert("hotspotGroups".to_string(), V::Array(sorted));
            }
            if let Some(V::Array(rows)) = out.get("rows") {
                let mut sorted: Vec<_> = rows.clone();
                sorted.sort_by(|a, b| {
                    let ak = a.get("activity_id").and_then(|v| v.as_str()).unwrap_or("");
                    let bk = b.get("activity_id").and_then(|v| v.as_str()).unwrap_or("");
                    ak.cmp(bk)
                });
                out.insert("rows".to_string(), V::Array(sorted));
            }
            // Sort object keys for deterministic snapshot (CI vs local HashMap order).
            let mut keys: Vec<_> = out.keys().cloned().collect();
            keys.sort();
            let sorted_map: serde_json::Map<_, _> = keys
                .into_iter()
                .map(|k| (k.clone(), out.remove(&k).unwrap()))
                .collect();
            V::Object(sorted_map)
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
        let stabilized = stabilize_ops_payload(redacted);
        if let Some(obj) = out.as_object_mut() {
            obj.insert("payload".to_string(), stabilized);
            obj.remove("payloadJson");
        }
    }
    stabilize_ops_payload(redact_runtime_fields(out))
}

async fn seeded_store_for_context(
    context_id: &ContextId,
    caller_agent: &AgentId,
    other_agent: &AgentId,
) -> Arc<baml_rt_provenance::GraphqliteProvenanceStore> {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "baml-tools-system-provenance-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let store = GraphqliteStoreBuilder::file(&path)
        .build()
        .expect("build store");
    let msg_caller = MessageId::from_external(ExternalId::new("tool-msg-caller".to_string()));
    let msg_other = MessageId::from_external(ExternalId::new("tool-msg-other".to_string()));
    let msg_linked = MessageId::from_external(ExternalId::new("tool-msg-linked".to_string()));
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
    let caller_failed_llm_event_id = EventId::from_counter(900);
    store
        .add_event(ProvEvent::Global(GlobalEvent {
            id: caller_failed_llm_event_id.clone(),
            context_id: context_id.clone(),
            timestamp_ms: 3,
            data: ProvEventData::LlmCallCompleted {
                scope: CallScope::Message {
                    message_id: msg_caller.clone(),
                },
                client: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                function_name: "CallerPrompt".to_string(),
                prompt: json!({"input":"caller-failed"}),
                // Sparse metadata on purpose; linked PromptRejected must classify this.
                metadata: call_metadata(caller_agent, &msg_caller, None),
                usage: LlmUsage::Known {
                    prompt_tokens: 7,
                    completion_tokens: 0,
                    total_tokens: 7,
                },
                duration_ms: 220,
                outcome: Outcome::Failure,
                drift: None,
            },
        }))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::prompt_rejected_global(
            context_id.clone(),
            msg_caller.clone(),
            caller_failed_llm_event_id,
            "BAML validation failed: missing required field".to_string(),
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::message_sent_global(
            context_id.clone(),
            msg_caller.clone(),
            "ROLE_AGENT".to_string(),
            vec!["BAML validation failed: missing required field".to_string()],
            None,
            caller_agent.clone(),
            4,
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
            msg_linked.clone(),
            "ROLE_USER".to_string(),
            vec!["linked evidence path".to_string()],
            None,
            caller_agent.clone(),
            5,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::tool_call_completed_global(
            context_id.clone(),
            msg_linked.clone(),
            "support/delegate".to_string(),
            Some("DelegatePrompt".to_string()),
            json!({"objective":"linked emitted message evidence"}),
            call_metadata(caller_agent, &msg_linked, None),
            330,
            Outcome::Failure,
            None,
        ))
        .await
        .unwrap();
    store
        .add_event(ProvEvent::message_sent_global(
            context_id.clone(),
            msg_linked,
            "ROLE_AGENT".to_string(),
            vec!["authentication failed: 401 unauthorized invalid api key".to_string()],
            None,
            caller_agent.clone(),
            6,
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
        description: description.map(str::to_string),
        capabilities: capabilities.into_iter().map(str::to_string).collect(),
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

    let step = registry.session_next(&session_id).await.unwrap();
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

    let step = registry.session_next(&session_id).await.unwrap();
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

    let step = registry.session_next(&session_id).await.unwrap();
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
    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "introspection_tool_output",
            snapshot_safe_tool_output(output.clone())
        );
    });
    let payload = output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .expect("payloadJson string");
    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "introspection_tool_request_scope",
            redact_runtime_fields(payload)
        );
    });
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
    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "extrospection_tool_output",
            snapshot_safe_tool_output(output.clone())
        );
    });
    let payload = output
        .get("payloadJson")
        .and_then(|v| v.as_str())
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .expect("payloadJson string");
    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "extrospection_tool_request_scope",
            redact_runtime_fields(payload)
        );
    });
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

    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "introspection_tool_pagination",
            redact_runtime_fields(serde_json::json!({
                "page1": snapshot_safe_tool_output(first_output),
                "page2": snapshot_safe_tool_output(second_output)
            }))
        );
    });
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

    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "extrospection_tool_filter_sort_drilldown",
            redact_runtime_fields(serde_json::json!({
                "filtered": snapshot_safe_tool_output(filtered_output),
                "drilldown": snapshot_safe_tool_output(drilldown_output)
            }))
        );
    });
}

#[tokio::test]
async fn extrospection_session_auto_drilldown_from_hotspot_snapshot() {
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
    let pass1 = registry.session_next(&session_id).await.unwrap();
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
    let pass2 = registry.session_next(&session_id).await.unwrap();
    let pass2_output = match pass2 {
        ToolStep::Done {
            output: Some(output),
        } => output,
        other => panic!("expected pass2 Done(Some(output)), got {:?}", other),
    };

    insta::with_settings!({sort_maps => true}, {
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
    });
}

#[tokio::test]
async fn extrospection_session_failure_evidence_linked_modes_snapshots() {
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
    let llm = registry.session_next(&session_id).await.unwrap();

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
    let tool = registry.session_next(&session_id).await.unwrap();

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

    insta::with_settings!({sort_maps => true}, {
        insta::assert_json_snapshot!(
            "extrospection_tool_failure_evidence_linked_modes",
            redact_runtime_fields(serde_json::json!({
                "llm_linked_prompt_rejected": snapshot_safe_tool_output(llm_output),
                "tool_linked_emitted_message": snapshot_safe_tool_output(tool_output)
            }))
        );
    });
}
