//! Tests for system discovery sessions: discover_agents and discover_tools via SystemBundle.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, AgentCard, AgentDiscoveryEntry, AgentLister, BusStream, ContextId, Result,
};
use baml_rt_tools::{ToolRegistry, ToolStep};
use baml_tools_calculator::CalculatorTool;
use baml_tools_system::SystemBundle;
use futures_util::stream;
use serde_json::{Value, json};

struct MockA2aHandler;

#[async_trait]
impl A2aRequestHandler for MockA2aHandler {
    async fn handle_a2a_stream(&self, _request: Value) -> Result<BusStream<Value>> {
        Ok(Box::pin(stream::empty()))
    }
}

struct MockAgentList {
    entries: Vec<AgentDiscoveryEntry>,
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

    let session_id = registry
        .open_session("system/discover_agents", json!({}), &ContextId::new(1, 1))
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

    let session_id = registry
        .open_session("system/discover_tools", json!({}), &ContextId::new(1, 2))
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
                names.iter().any(|n| *n == "support/calculate"),
                "expected support/calculate in {:?}",
                names
            );
            assert_eq!(output.get("done").and_then(|v| v.as_bool()), Some(true));
        }
        other => panic!("expected Done(Some(output)), got {:?}", other),
    }
}
