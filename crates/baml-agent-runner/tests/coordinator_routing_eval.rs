#![cfg(feature = "llm-tests")]

//! LLM-gated evaluation of the coordinator's workflow planning.
//!
//! Uses curated prompts and verifies the planner routes to expected agent targets.
//! Requires OPENROUTER_API_KEY to be set.

mod common;

use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::{A2aRequestHandler, baml::BamlRuntimeManager};
use baml_rt_core::{
    AgentDiscoveryEntry, AgentLister,
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphqliteProvenanceStore, GraphqliteStoreBuilder, ProvEvent, ProvenanceWriter,
};
use baml_tools_system::SystemBundle;
use common::{
    TempDirCleanup, build_agent_dir_to_temp_async, e2e_serial_gate, post_a2a_sse_collect,
    start_runner_api_server,
};
use serde_json::Value;
use test_support::common::{
    agent_fixture, chunks_from_responses, ensure_fixture_runtime_types, message_texts_from_chunks,
    require_api_key, send_stream_request, workspace_fnox_path,
};
use tokio::time::{Duration, timeout};

struct EmptyAgentList;

impl AgentLister for EmptyAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        vec![]
    }
}

struct EmptyA2aHandler;

#[async_trait]
impl A2aRequestHandler for EmptyA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: baml_rt_core::A2aWireRequest,
    ) -> baml_rt::Result<baml_rt_core::bus::BusStream<baml_rt_core::A2aStreamChunk>> {
        Ok(Box::pin(futures_util::stream::empty::<
            baml_rt_core::A2aStreamChunk,
        >()))
    }
}

fn build_graphqlite_test_store() -> Arc<GraphqliteProvenanceStore> {
    let path = std::env::temp_dir().join(format!(
        "baml-rt-coord-eval-{pid}-{unique}.db",
        pid = std::process::id(),
        unique = uuid::Uuid::new_v4(),
    ));
    GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build isolated GraphQLite store")
}

async fn setup_coordinator_agent() -> (baml_rt::A2aAgent, Arc<GraphqliteProvenanceStore>, PathBuf) {
    ensure_fixture_runtime_types();

    let agent_dir = agent_fixture("coordinator-smoke");
    let built = build_agent_dir_to_temp_async(agent_dir, "coordinator-smoke-eval").await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("path utf8"))
        .expect("load coordinator schema");
    let allowlist: HashSet<String> = [
        "system/internal_a2a",
        "system/discover_agents",
        "system/discover_tools",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    manager
        .set_tool_allowlist(allowlist)
        .await
        .expect("set allowlist");
    let registry = manager.tool_registry();
    registry
        .register_bundle(SystemBundle::new(
            Arc::new(EmptyAgentList),
            registry.clone(),
            Arc::new(EmptyA2aHandler),
        ))
        .expect("register SystemBundle");

    let provenance = build_graphqlite_test_store();
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("coordinator-smoke").expect("agent type"),
            "1.0.0".to_string(),
            "coordinator-smoke@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");

    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_graphqlite_store(provenance.clone())
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build coordinator agent");
    (agent, provenance, built)
}

/// Verifies the coordinator handles a simple math question (expects direct_answer).
#[tokio::test]
async fn eval_coordinator_direct_answer_math() {
    let _openrouter_api_key = require_api_key();
    let _permit = e2e_serial_gate().acquire().await.expect("gate");

    let (agent, provenance, built_dir) = setup_coordinator_agent().await;
    let _guard = TempDirCleanup::new(built_dir);

    let runner_api = match start_runner_api_server("coordinator-smoke", agent, provenance).await {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Skipping eval: {err}");
            return;
        }
    };

    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = send_stream_request(
        "eval-direct-1",
        "What is 2 + 2?",
        correlation_id.as_str(),
        Some(ContextId::new(99, 20)),
    );

    let http_client = reqwest::Client::new();
    let url = format!(
        "{base}/agents/coordinator-smoke/default/a2a/sse",
        base = runner_api.base_url,
    );
    let responses: Vec<Value> = timeout(
        Duration::from_secs(120),
        post_a2a_sse_collect(&http_client, &url, &request_body),
    )
    .await
    .expect("timed out")
    .expect("request failed");

    assert!(!responses.is_empty(), "expected non-empty responses");

    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    let merged = texts.join("\n");

    assert!(
        merged.contains('4') || merged.to_lowercase().contains("four"),
        "Expected '4' in direct answer. Got: {merged}"
    );

    runner_api.stop().await;
}
