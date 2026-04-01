#![cfg(feature = "llm-tests")]

mod common;

use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::{A2aRequestHandler, baml::BamlRuntimeManager};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentLister,
    bus::{BusStream, BusWithEffects},
};
use baml_rt_provenance::SurrealProvenanceStore;
use baml_tools_system::SystemBundle;
use common::{
    TempDirCleanup, build_agent_dir_to_temp_async, e2e_serial_gate, start_runner_api_server,
};
use serde_json::{Value, json};
use test_support::common::{
    fnox_has_openrouter_key, test_surreal_store, workspace_fnox_path, workspace_root,
};
use tokio::{
    sync::Mutex,
    time::{Duration, timeout},
};

#[derive(Clone)]
struct StaticAgentList {
    entries: Vec<AgentDiscoveryEntry>,
}

impl AgentLister for StaticAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DelegationCall {
    agent_package: String,
    agent_instance_id: String,
    prompt: String,
}

#[derive(Clone, Default)]
struct CapturingA2aHandler {
    calls: Arc<Mutex<Vec<DelegationCall>>>,
}

impl CapturingA2aHandler {
    async fn snapshot_calls(&self) -> Vec<DelegationCall> {
        self.calls.lock().await.clone()
    }
}

#[async_trait]
impl A2aRequestHandler for CapturingA2aHandler {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> baml_rt::Result<BusStream<A2aStreamChunk>> {
        let target_package = request
            .as_ref()
            .pointer("/params/metadata/target/agent_package")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let target_instance = request
            .as_ref()
            .pointer("/params/metadata/target/agent_instance_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let prompt = request
            .as_ref()
            .pointer("/params/message/parts/0/text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        self.calls.lock().await.push(DelegationCall {
            agent_package: target_package.clone(),
            agent_instance_id: target_instance,
            prompt,
        });

        let response = json!({
            "result": {
                "message": {
                    "parts": [
                        {
                            "text": format!("delegated to {target_package}")
                        }
                    ]
                }
            }
        });

        Ok(Box::pin(futures_util::stream::iter(vec![
            A2aStreamChunk::from(response),
        ])))
    }
}

fn discovery_entry(package: &str, capabilities: &[&str]) -> AgentDiscoveryEntry {
    let card = AgentCard {
        name: package.to_string(),
        version: "1.0.0".to_string(),
        content_hash: None,
        repository_version: None,
        agent_package: package.to_string(),
        agent_instance_id: "default".to_string(),
        tools: Vec::new(),
        baml_functions: Vec::new(),
        description: Some(format!("{package} test agent")),
        capabilities: capabilities
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        tags: Vec::new(),
        subscriptions: Vec::new(),
    };

    AgentDiscoveryEntry {
        agent_package: package.to_string(),
        agent_instance_id: "default".to_string(),
        name: package.to_string(),
        version: "1.0.0".to_string(),
        agent_card: card,
    }
}

async fn build_workspace_semantic_ingress_agent() -> PathBuf {
    let agent_dir = workspace_root()
        .join("agents")
        .join("semantic-ingress-agent");
    build_agent_dir_to_temp_async(agent_dir, "semantic-ingress-agent").await
}

async fn setup_semantic_ingress_agent(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (baml_rt::A2aAgent, Arc<SurrealProvenanceStore>, PathBuf) {
    let built = build_workspace_semantic_ingress_agent().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("semantic-ingress built path utf8"))
        .expect("load semantic-ingress schema");

    let allowlist: HashSet<String> = [
        "system/callback",
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
        .register_bundle(SystemBundle::new(agent_list, registry.clone(), a2a_handler))
        .expect("register SystemBundle");

    let provenance = test_surreal_store().await;
    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("semantic-ingress dist");
    let agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(provenance.clone())
        .build()
        .await
        .expect("build semantic-ingress agent");

    (agent, provenance, built)
}

fn raw_slack_source_event(source_label: &str, records: Vec<Value>) -> Value {
    let source_suffix = source_label
        .trim_start_matches('#')
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .replace('_', "")
        .to_uppercase();

    json!({
        "schema_version": "host.source-records.v1",
        "emitted_at_unix": 1_735_720_111u64,
        "source": {
            "source_kind": "slack",
            "source_key": format!("slack:C{source_suffix}"),
            "source_label": source_label
        },
        "records": records
    })
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_batches_actionable_threads_into_one_downstream_route() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    if !fnox_has_openrouter_key() {
        eprintln!(
            "Skipping semantic_ingress_dispatch_http_batches_actionable_threads_into_one_downstream_route: OPENROUTER_API_KEY not resolved from fnox.toml"
        );
        return;
    }

    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry("clickup-agent", &["clickup:create-task"])],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (agent, provenance, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api =
        match start_runner_api_server("semantic-ingress-agent", agent, provenance).await {
            Ok(server) => server,
            Err(err) => {
                eprintln!(
                    "Skipping semantic-ingress dispatch e2e: cannot bind runner API server: {err}"
                );
                return;
            }
        };

    let client = reqwest::Client::new();
    let dispatch_url = format!(
        "{}/agents/semantic-ingress-agent/default/dispatch",
        runner_api.base_url
    );
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "context_id": "ctx-1735720000000-semantic-ingress-1",
        "task_id": "dispatch-task-1735720000000-semantic-ingress",
        "message_id": "dispatch-msg-1735720000000-semantic-ingress",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![
                    json!({
                        "ts": "1735720311.000001",
                        "user": "U123",
                        "user_name": "Ada",
                        "text": "Please create a task for the flaky test fix."
                    }),
                    json!({
                        "ts": "1735720312.000001",
                        "thread_ts": "1735720311.000001",
                        "user": "U456",
                        "user_name": "Grace",
                        "text": "This is blocking the rollout until we follow up."
                    }),
                    json!({
                        "ts": "1735720411.000001",
                        "user": "U789",
                        "user_name": "Linus",
                        "text": "Please open a separate task for release notes."
                    })
                ]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(240),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress dispatch timed out")
    .expect("semantic-ingress dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Processed 2 thread(s); routed 2 actionable thread(s)."),
        "expected batched thread summary in dispatch ack, got: {ack:?}"
    );

    let calls = handler.snapshot_calls().await;
    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation for a multi-thread source batch; ack={ack:?}; calls={calls:?}"
    );
    assert_eq!(calls[0].agent_package, "clickup-agent");
    assert_eq!(calls[0].agent_instance_id, "default");
    assert!(
        calls[0].prompt.contains("Actionable Threads: 2"),
        "expected combined delegation prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("Thread 1: 1735720311.000001"),
        "expected first thread key in combined prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("Thread 2: 1735720411.000001"),
        "expected second thread key in combined prompt, got: {}",
        calls[0].prompt
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_rejects_noncanonical_raw_source_routing_key() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry("clickup-agent", &["clickup:create-task"])],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (agent, provenance, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = match start_runner_api_server("semantic-ingress-agent", agent, provenance)
        .await
    {
        Ok(server) => server,
        Err(err) => {
            eprintln!(
                "Skipping semantic-ingress routing-key guard test: cannot bind runner API server: {err}"
            );
            return;
        }
    };

    let client = reqwest::Client::new();
    let dispatch_url = format!(
        "{}/agents/semantic-ingress-agent/default/dispatch",
        runner_api.base_url
    );
    let dispatch_body = json!({
        "routing_key": "slack:intake",
        "message_type": "host.source-records.v1",
        "context_id": "ctx-1735720000000-semantic-ingress-bad-routing",
        "task_id": "dispatch-task-1735720000000-semantic-ingress-bad-routing",
        "message_id": "dispatch-msg-1735720000000-semantic-ingress-bad-routing",
        "messages": [
            raw_slack_source_event("#agentium-eng", vec![])
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress bad-routing dispatch timed out")
    .expect("semantic-ingress bad-routing dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress bad-routing dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("semantic-ingress-agent expected routing_key event:intake"),
        "expected routing-key rejection detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "unexpected downstream delegation for rejected routing key"
    );

    runner_api.stop().await;
}
