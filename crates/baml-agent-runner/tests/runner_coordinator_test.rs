#![cfg(feature = "llm-tests")]

mod common;

use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

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
use serde_json::{Value, json};
use test_support::common::{
    agent_fixture, chunks_from_responses, ensure_fixture_runtime_types, message_texts_from_chunks,
    require_api_key, send_stream_request, workspace_root,
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
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "baml-rt-runner-coordinator-{pid}-{unique}.db",
        pid = std::process::id(),
    ));
    GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build isolated GraphQLite store")
}

async fn build_coordinator_smoke_fixture() -> PathBuf {
    let agent_dir = agent_fixture("coordinator-smoke");
    build_agent_dir_to_temp_async(agent_dir, "coordinator-smoke").await
}

async fn build_workspace_coordinator_agent() -> PathBuf {
    let agent_dir = workspace_root().join("agents").join("coordinator-agent");
    build_agent_dir_to_temp_async(agent_dir, "coordinator-agent").await
}

async fn setup_coordinator_agent_with_provenance()
-> (baml_rt::A2aAgent, Arc<GraphqliteProvenanceStore>, PathBuf) {
    ensure_fixture_runtime_types();

    let built = build_coordinator_smoke_fixture().await;
    let mut manager = BamlRuntimeManager::new().expect("create manager");
    manager
        .load_schema(built.to_str().expect("coordinator built path utf8"))
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

    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("coordinator-smoke dist/index.js");
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

async fn setup_workspace_coordinator_with_provenance()
-> (baml_rt::A2aAgent, Arc<GraphqliteProvenanceStore>, PathBuf) {
    ensure_fixture_runtime_types();

    let built = build_workspace_coordinator_agent().await;
    let mut manager = BamlRuntimeManager::new().expect("create manager");
    manager
        .load_schema(built.to_str().expect("coordinator built path utf8"))
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
            AgentType::new("coordinator-agent").expect("agent type"),
            "1.0.0".to_string(),
            "coordinator-agent@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");

    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("coordinator-agent dist/index.js");
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

/// LLM-gated integration test: builds the coordinator-smoke fixture, starts it
/// as an A2A agent, sends a simple query, and verifies it produces a response
/// (either via direct_answer or delegation to discovered agents).
#[tokio::test]
async fn test_coordinator_smoke_direct_answer() {
    let _openrouter_api_key = require_api_key();
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (agent, _provenance, built_dir) = setup_coordinator_agent_with_provenance().await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api =
        match start_runner_api_server("coordinator-smoke", agent, _provenance.clone()).await {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Skipping coordinator test: cannot bind runner API server: {err}");
                return;
            }
        };

    let context_id = ContextId::new(99, 10);
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = send_stream_request(
        "coord-smoke-1",
        "What is 2 + 2?",
        correlation_id.as_str(),
        Some(context_id.clone()),
    );

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{base}/agents/coordinator-smoke/default/a2a/sse",
        base = runner_api.base_url,
    );
    let responses: Vec<Value> = timeout(
        Duration::from_secs(120),
        post_a2a_sse_collect(&http_client, &a2a_url, &request_body),
    )
    .await
    .expect("coordinator A2A SSE request timed out")
    .expect("coordinator A2A SSE request failed");

    assert!(
        !responses.is_empty(),
        "Expected non-empty JSON-RPC response array from coordinator /a2a/sse"
    );

    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    let merged_text = texts.join("\n");
    assert!(
        !merged_text.is_empty(),
        "Expected non-empty response text from coordinator. Chunks: {chunks:?}"
    );

    // The coordinator should handle "2+2" via direct_answer or a simple synthesis
    assert!(
        merged_text.contains('4') || merged_text.to_lowercase().contains("four"),
        "Expected response to contain '4' for a simple math query. Got: {merged_text}"
    );

    runner_api.stop().await;
}

/// Verifies coordinator accepts a typed task-daemon handoff when no text part
/// is present in the inbound message.
#[tokio::test]
async fn test_coordinator_accepts_data_only_task_daemon_handoff() {
    let _openrouter_api_key = require_api_key();

    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (agent, _provenance, built_dir) = setup_workspace_coordinator_with_provenance().await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api =
        match start_runner_api_server("coordinator-agent", agent, _provenance.clone()).await {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Skipping coordinator test: cannot bind runner API server: {err}");
                return;
            }
        };

    let context_id = ContextId::new(99, 11);
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = json!({
        "jsonrpc": "2.0",
        "method": "message.sendStream",
        "id": correlation_id.as_str(),
        "params": {
            "message": {
                "messageId": "coord-smoke-data-only-msg-1",
                "role": "user",
                "parts": [
                    {
                        "data": {
                            "schema_version": "task-daemon.coordinator-handoff.v1",
                            "batch": {
                                "source": "slack",
                                "source_label": "#agentium-eng",
                                "generated_at_unix": 1_735_720_000u64,
                                "messages_scanned": 3,
                                "project": {
                                    "project_key": "agent-platform",
                                    "repo_available": true,
                                    "repo_path": "/repo/agent-platform"
                                },
                                "interpretation": {
                                    "executive_summary": "Team needs investigation tasks created from discussion context.",
                                    "current_objectives": [
                                        "Convert structured interpretation into a runnable workflow"
                                    ],
                                    "workflow_seed": {
                                        "goal": "Create investigation tasks and route follow-ups",
                                        "investigation_nodes": [
                                            {
                                                "key": "investigate-routing",
                                                "title": "Investigate routing behavior",
                                                "goal": "Validate coordinator consumption path",
                                                "prompt": "Inspect coordinator intake and prove structured handoff is used.",
                                                "when_to_run": "repo_available"
                                            }
                                        ]
                                    }
                                },
                                "derived_tasks": [
                                    {
                                        "key": "task-1",
                                        "title": "Validate typed handoff ingestion",
                                        "description": "Ensure coordinator can plan from parts[].data without text fallback.",
                                        "priority": "high"
                                    }
                                ]
                            }
                        }
                    }
                ],
                "contextId": context_id.to_string()
            }
        }
    });

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{base}/agents/coordinator-agent/default/a2a/sse",
        base = runner_api.base_url,
    );
    let responses: Vec<Value> = timeout(
        Duration::from_secs(120),
        post_a2a_sse_collect(&http_client, &a2a_url, &request_body),
    )
    .await
    .expect("coordinator A2A SSE request timed out")
    .expect("coordinator A2A SSE request failed");

    assert!(
        !responses
            .iter()
            .any(|response| response.get("error").is_some()),
        "Expected no JSON-RPC error envelopes. Responses: {responses:?}"
    );

    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    let merged_text = texts.join("\n");

    assert!(
        texts.iter().any(|text| {
            text.contains(
                "Received structured task-daemon handoff. Planning from interpretation payload.",
            )
        }),
        "Expected coordinator to confirm structured handoff path. Texts: {texts:?}"
    );
    assert!(
        !merged_text.contains("Please share what you want me to coordinate."),
        "Coordinator should not reject data-only handoff as empty input. Texts: {texts:?}"
    );
    assert!(
        !merged_text.contains("Agent discovery failed:"),
        "Coordinator should be able to continue past discovery for this test. Texts: {texts:?}"
    );
    assert!(
        !merged_text.contains("Workflow planning failed:"),
        "Coordinator should produce a planned response from the structured handoff. Texts: {texts:?}"
    );
    assert!(
        !merged_text.trim().is_empty(),
        "Expected non-empty response stream from coordinator. Responses: {responses:?}"
    );

    runner_api.stop().await;
}
