mod common;

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::{A2aRequestHandler, BamlRtError, baml::BamlRuntimeManager};
use baml_rt_a2a::{AgentRegistry, EventDispatcher};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentLister, AgentRouteKey, EventSchemaVersion, EventSourceKind,
    bus::{BusStream, BusWithEffects},
    event_subscription::EventSubscription,
};
use baml_rt_tools::{
    BundleName, ConfigResolver, InventoryCatalog, load_configured_event_producers_with_checkpoints,
};
use baml_tools_slack::SlackTool;
use baml_tools_system::SystemBundle;
use common::{
    RunningHttpServer, build_agent_dir_to_temp_async, e2e_serial_gate, start_http_server,
};
use serde_json::{Value, json};
use test_support::common::{
    TempDirCleanup, TempEnvVar, test_surreal_store, workspace_fnox_path, workspace_root,
};
use tokio::{
    sync::Mutex,
    time::{Duration, timeout},
};

// Inventory config keys use the bundle registration name rather than the tool path.
const SLACK_BUNDLE_NAME: &str = "support_slack";
const PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY: &str = "project-management:create-task";

#[derive(Clone)]
struct StaticAgentList {
    entries: Vec<AgentDiscoveryEntry>,
}

impl AgentLister for StaticAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

struct StaticConfigResolver {
    configs: HashMap<String, Value>,
}

#[async_trait]
impl ConfigResolver for StaticConfigResolver {
    async fn get_config(&self, bundle_name: &BundleName) -> baml_rt_core::Result<Option<Value>> {
        Ok(self.configs.get(bundle_name.as_str()).cloned())
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

#[derive(Clone, Default)]
struct FailingA2aHandler;

#[async_trait]
impl A2aRequestHandler for FailingA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> baml_rt::Result<BusStream<A2aStreamChunk>> {
        Err(BamlRtError::InvalidArgument(
            "downstream agent unavailable".to_string(),
        ))
    }
}

#[derive(Clone)]
struct StreamingA2aHandler {
    chunks: Vec<Value>,
}

#[async_trait]
impl A2aRequestHandler for StreamingA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> baml_rt::Result<BusStream<A2aStreamChunk>> {
        let chunks = self
            .chunks
            .clone()
            .into_iter()
            .map(A2aStreamChunk::from)
            .collect::<Vec<_>>();
        Ok(Box::pin(futures_util::stream::iter(chunks)))
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

async fn setup_semantic_ingress_agent_unlocked(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (baml_rt::A2aAgent, PathBuf) {
    let built = build_workspace_semantic_ingress_agent().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("semantic-ingress built path utf8"))
        .expect("load semantic-ingress schema");

    let allowlist = [
        "support/slack",
        // SystemBundle currently registers the callback tool alongside discovery and A2A.
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
    manager
        .register_tool(SlackTool::new())
        .await
        .expect("register SlackTool");

    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("semantic-ingress dist");
    let agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(test_surreal_store().await)
        .build()
        .await
        .expect("build semantic-ingress agent");

    (agent, built)
}

async fn setup_semantic_ingress_agent(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (
    tokio::sync::SemaphorePermit<'static>,
    baml_rt::A2aAgent,
    PathBuf,
) {
    let permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (agent, built) = setup_semantic_ingress_agent_unlocked(agent_list, a2a_handler).await;
    (permit, agent, built)
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

fn semantic_ingress_dispatch_url(base_url: &str) -> String {
    format!(
        "{base}/agents/semantic-ingress-agent/default/dispatch",
        base = base_url
    )
}

fn mock_api_base_url(base_url: &str) -> String {
    format!("{base}/api", base = base_url)
}

#[derive(Clone)]
struct DispatchRegistry {
    package: String,
    instance_id: String,
    name: String,
    version: String,
    subscriptions: Vec<EventSubscription>,
    agent: baml_rt::A2aAgent,
}

impl DispatchRegistry {
    fn new(
        package: &str,
        instance_id: &str,
        name: &str,
        version: &str,
        agent: baml_rt::A2aAgent,
    ) -> Self {
        Self {
            package: package.to_string(),
            instance_id: instance_id.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            subscriptions: Vec::new(),
            agent,
        }
    }

    fn with_subscriptions(mut self, subscriptions: Vec<EventSubscription>) -> Self {
        self.subscriptions = subscriptions;
        self
    }
}

#[async_trait]
impl AgentLister for DispatchRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let agent_card = AgentCard {
            name: self.name.clone(),
            version: self.version.clone(),
            content_hash: None,
            repository_version: None,
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            tools: Vec::new(),
            baml_functions: Vec::new(),
            description: None,
            capabilities: Vec::new(),
            tags: Vec::new(),
            subscriptions: self.subscriptions.clone(),
        };
        vec![AgentDiscoveryEntry {
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            agent_card,
        }]
    }
}

#[async_trait]
impl AgentRegistry for DispatchRegistry {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> baml_rt_core::Result<BusStream<A2aStreamChunk>> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            let pkg = key.agent_package.as_str();
            let inst = key.agent_instance_id.as_str();
            return Err(BamlRtError::InvalidArgument(format!(
                "Agent {pkg}/{inst} not found",
            )));
        }
        self.agent.handle_a2a_stream(request).await
    }

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> baml_rt_core::Result<AgentDispatchAck> {
        if key.agent_package.as_str() != self.package
            || key.agent_instance_id.as_str() != self.instance_id
        {
            let pkg = key.agent_package.as_str();
            let inst = key.agent_instance_id.as_str();
            return Err(BamlRtError::InvalidArgument(format!(
                "Agent {pkg}/{inst} not found",
            )));
        }
        self.agent.handle_dispatch(request).await
    }
}

async fn start_runner_api_server(
    agent_package: &str,
    agent: baml_rt::A2aAgent,
) -> std::io::Result<RunningHttpServer> {
    let registry = Arc::new(DispatchRegistry::new(
        agent_package,
        "default",
        agent_package,
        "1.0.0",
        agent,
    )) as Arc<dyn AgentRegistry>;
    let app = baml_rt_api::api_router(registry, None, None).await;
    start_http_server(app, None).await
}

#[derive(Clone)]
struct MockSlackProducerState {
    hits: Arc<Mutex<Vec<String>>>,
    history_body: Arc<Value>,
    replies_body: Arc<Value>,
}

impl MockSlackProducerState {
    async fn snapshot_hits(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_slack_producer_server(
    history_messages: Vec<Value>,
    reply_messages: Vec<Value>,
) -> std::io::Result<(RunningHttpServer, MockSlackProducerState)> {
    use axum::{
        Json, Router,
        extract::{OriginalUri, State as AxumState},
        routing::get,
    };

    async fn conversation_history(
        AxumState(state): AxumState<MockSlackProducerState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET {uri}", uri = uri.0));
        Json((*state.history_body).clone())
    }

    async fn thread_replies(
        AxumState(state): AxumState<MockSlackProducerState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET {uri}", uri = uri.0));
        Json((*state.replies_body).clone())
    }

    let state = MockSlackProducerState {
        hits: Arc::new(Mutex::new(Vec::new())),
        history_body: Arc::new(json!({
            "ok": true,
            "messages": history_messages,
            "has_more": false,
            "response_metadata": { "next_cursor": "" }
        })),
        replies_body: Arc::new(json!({
            "ok": true,
            "messages": reply_messages,
            "has_more": false,
            "response_metadata": { "next_cursor": "" }
        })),
    };
    let app = Router::new()
        .route("/api/conversations.history", get(conversation_history))
        .route("/api/conversations.replies", get(thread_replies))
        .with_state(state.clone());
    let server = start_http_server(app, None).await?;
    Ok((server, state))
}

#[derive(Clone)]
struct MockSlackRepliesState {
    hits: Arc<Mutex<Vec<String>>>,
    response_body: Arc<Value>,
}

impl MockSlackRepliesState {
    async fn snapshot_hits(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_slack_thread_replies_server(
    response_messages: Vec<Value>,
) -> std::io::Result<(RunningHttpServer, MockSlackRepliesState)> {
    use axum::{
        Json, Router,
        extract::{OriginalUri, State as AxumState},
        routing::get,
    };

    async fn thread_replies(
        AxumState(state): AxumState<MockSlackRepliesState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET {uri}", uri = uri.0));
        Json((*state.response_body).clone())
    }

    let state = MockSlackRepliesState {
        hits: Arc::new(Mutex::new(Vec::new())),
        response_body: Arc::new(json!({
            "ok": true,
            "messages": response_messages,
            "has_more": false,
            "response_metadata": { "next_cursor": "" }
        })),
    };
    let app = Router::new()
        .route("/api/conversations.replies", get(thread_replies))
        .with_state(state.clone());
    let server = start_http_server(app, None).await?;
    Ok((server, state))
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_routes_raw_slack_source_records_to_task_management_creation_capability()
 {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![
                    json!({
                        "ts": "1735720111.000001",
                        "user": "U123",
                        "user_name": "Ada",
                        "text": "Please turn this Slack thread into a tracked task."
                    }),
                    json!({
                        "ts": "1735720112.000001",
                        "user": "U456",
                        "user_name": "Grace",
                        "text": "This is blocking the rollout until we follow up."
                    })
                ]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
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
        detail.contains("Routed create_pm_work to clickup-agent/default."),
        "expected routed create_pm_work detail, got: {ack:?}"
    );

    let calls = handler.snapshot_calls().await;
    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; calls={calls:?}"
    );
    assert_eq!(calls[0].agent_package, "clickup-agent");
    assert!(
        calls[0]
            .prompt
            .contains("Ingress kind: Slack semantic ingress from raw source records"),
        "expected semantic-ingress prompt header, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0]
            .prompt
            .contains("Please turn this Slack thread into a tracked task."),
        "expected transcript details in prompt, got: {}",
        calls[0].prompt
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_fails_when_no_task_management_sink_matches() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720140.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Please create a tracked task for the deployment follow-up."
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress no-sink dispatch timed out")
    .expect("semantic-ingress no-sink dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("semantic-ingress no-sink ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("No downstream agent matched required capabilities:")
            && detail.contains(PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY),
        "expected no-sink detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "unexpected downstream delegation when no task-management sink matched"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_rejects_ambiguous_task_management_sinks() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![
            discovery_entry(
                "clickup-agent",
                &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
            ),
            discovery_entry("linear-agent", &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY]),
        ],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720141.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Please create a tracked task for the ambiguous sink case."
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress ambiguous-sink dispatch timed out")
    .expect("semantic-ingress ambiguous-sink dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress ambiguous-sink ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Multiple downstream agents matched required capabilities")
            && detail.contains(PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY)
            && detail.contains("clickup-agent/default")
            && detail.contains("linear-agent/default"),
        "expected ambiguous-sink detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "unexpected downstream delegation when task-management sink selection was ambiguous"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_fails_when_downstream_agent_errors() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(FailingA2aHandler);
    let (_permit, agent, built_dir) = setup_semantic_ingress_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720151.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Please create a task for the downstream error case."
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress downstream-error dispatch timed out")
    .expect("semantic-ingress downstream-error request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress downstream-error ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("semantic-ingress-agent failed:")
            && detail.contains("downstream agent unavailable"),
        "expected downstream error detail, got: {ack:?}"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_fails_when_delegated_child_task_reports_failed_state() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(StreamingA2aHandler {
        chunks: vec![
            json!({
                "statusUpdate": {
                    "status": { "state": "TASK_STATE_WORKING" }
                }
            }),
            json!({
                "task": {
                    "status": {
                        "state": "TASK_STATE_FAILED",
                        "message": {
                            "parts": [{ "text": "Delegated ClickUp workflow failed after streaming started." }]
                        }
                    }
                }
            }),
        ],
    });
    let (_permit, agent, built_dir) = setup_semantic_ingress_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720152.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Please create a task for the delegated failure case."
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress delegated-failed-state dispatch timed out")
    .expect("semantic-ingress delegated-failed-state request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress delegated-failed-state ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Delegated ClickUp workflow failed after streaming started."),
        "expected delegated child-task failure detail, got: {ack:?}"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_fails_when_delegated_child_task_requires_follow_up_input() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(StreamingA2aHandler {
        chunks: vec![json!({
            "task": {
                "status": {
                    "state": "TASK_STATE_INPUT_REQUIRED",
                    "message": {
                        "parts": [{ "text": "Need human confirmation before continuing the delegated ClickUp workflow." }]
                    }
                }
            }
        })],
    });
    let (_permit, agent, built_dir) = setup_semantic_ingress_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720153.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Please create a task for the follow-up-input case."
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress input-required dispatch timed out")
    .expect("semantic-ingress input-required request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress input-required ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail
            .contains("Need human confirmation before continuing the delegated ClickUp workflow."),
        "expected delegated input-required detail, got: {ack:?}"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_noops_raw_slack_chatter_without_action_cues() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![
                    json!({
                        "ts": "1735720211.000001",
                        "user": "U123",
                        "user_name": "Ada",
                        "text": "Heads up, CI is green again."
                    }),
                    json!({
                        "ts": "1735720212.000001",
                        "thread_ts": "1735720211.000001",
                        "user": "U456",
                        "user_name": "Grace",
                        "text": "Thanks for the update."
                    })
                ]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress noop dispatch timed out")
    .expect("semantic-ingress noop dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("semantic-ingress noop ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("did not find a clear actionable request"),
        "expected noop explanation, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "expected no downstream delegation for chatter"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_batches_mixed_raw_slack_work_items_into_one_delegation() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
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
                        "ts": "1735720411.000001",
                        "user": "U456",
                        "user_name": "Grace",
                        "text": "Please open a separate task for release notes."
                    })
                ]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress mixed dispatch timed out")
    .expect("semantic-ingress mixed dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("semantic-ingress mixed ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));

    let calls = handler.snapshot_calls().await;
    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; calls={calls:?}"
    );
    assert_eq!(calls[0].agent_package, "clickup-agent");
    assert!(
        calls[0].prompt.contains("Derived tasks (2 total):"),
        "expected two derived tasks in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0]
            .prompt
            .contains("Please create a task for the flaky test fix."),
        "expected first conversation in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0]
            .prompt
            .contains("Please open a separate task for release notes."),
        "expected second conversation in prompt, got: {}",
        calls[0].prompt
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_expands_slack_threads_before_deriving_work() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (mock_server, mock_state) = start_slack_thread_replies_server(vec![
        json!({
            "channel_id": "Cagentiumeng",
            "ts": "1735720511.000001",
            "thread_ts": "1735720511.000001",
            "user_id": "U123",
            "user_name": "Ada",
            "text": "Can we track the OAuth docs follow-up?"
        }),
        json!({
            "channel_id": "Cagentiumeng",
            "ts": "1735720512.000001",
            "thread_ts": "1735720511.000001",
            "user_id": "U456",
            "user_name": "Grace",
            "text": "Please create a task for the OAuth runbook and assign an owner."
        }),
    ])
    .await
    .expect("start slack thread replies fixture");
    let _env_slack_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb_test_slack_fixture");
    let _env_slack_base = TempEnvVar::set(
        "SLACK_API_BASE_URL",
        &mock_api_base_url(&mock_server.base_url),
    );
    let _env_slack_user = TempEnvVar::remove("SLACK_USER_TOKEN");

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (agent, built_dir) =
        setup_semantic_ingress_agent_unlocked(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720511.000001",
                    "thread_ts": "1735720511.000001",
                    "reply_count": 1,
                    "latest_reply": "1735720512.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Can we track the OAuth docs follow-up?"
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress thread-expansion dispatch timed out")
    .expect("semantic-ingress thread-expansion dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress thread-expansion ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));

    let calls = handler.snapshot_calls().await;
    let hits = mock_state.snapshot_hits().await;
    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; calls={calls:?}"
    );
    assert!(
        calls[0]
            .prompt
            .contains("Please create a task for the OAuth runbook and assign an owner."),
        "expected expanded thread transcript in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("including 1 expanded thread"),
        "expected summary to note thread expansion, got: {}",
        calls[0].prompt
    );
    assert!(
        hits.iter()
            .any(|hit| hit.contains("/api/conversations.replies")),
        "expected Slack thread-replies fetch, hits={hits:?}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_does_not_expand_thread_root_without_replies() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (mock_server, mock_state) = start_slack_thread_replies_server(Vec::new())
        .await
        .expect("start slack thread replies fixture");
    let _env_slack_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb_test_slack_fixture");
    let _env_slack_base = TempEnvVar::set(
        "SLACK_API_BASE_URL",
        &mock_api_base_url(&mock_server.base_url),
    );
    let _env_slack_user = TempEnvVar::remove("SLACK_USER_TOKEN");

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (agent, built_dir) =
        setup_semantic_ingress_agent_unlocked(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");
    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event(
                "#agentium-eng",
                vec![json!({
                    "ts": "1735720511.000001",
                    "thread_ts": "1735720511.000001",
                    "user": "U123",
                    "user_name": "Ada",
                    "text": "Please create a task for the OAuth docs follow-up."
                })]
            )
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress root-only dispatch timed out")
    .expect("semantic-ingress root-only dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress root-only dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));

    let calls = handler.snapshot_calls().await;
    let hits = mock_state.snapshot_hits().await;
    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; calls={calls:?}"
    );
    assert!(
        calls[0]
            .prompt
            .contains("Please create a task for the OAuth docs follow-up."),
        "expected root message in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        !calls[0].prompt.contains("expanded thread"),
        "did not expect root-only message to report thread expansion, got: {}",
        calls[0].prompt
    );
    assert!(
        hits.is_empty(),
        "expected no Slack thread-replies fetch for root-only message, hits={hits:?}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}

#[tokio::test]
async fn slack_producer_poll_and_deliver_reaches_semantic_ingress_and_downstream_delegation() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");

    let (mock_server, mock_state) = start_slack_producer_server(
        vec![json!({
            "type": "message",
            "user": "U123",
            "text": "Can we track the OAuth docs follow-up?",
            "ts": "1735720511.000001",
            "thread_ts": "1735720511.000001",
            "reply_count": 1,
            "latest_reply": "1735720512.000001"
        })],
        vec![
            json!({
                "type": "message",
                "user": "U123",
                "text": "Can we track the OAuth docs follow-up?",
                "ts": "1735720511.000001",
                "thread_ts": "1735720511.000001"
            }),
            json!({
                "type": "message",
                "user": "U456",
                "text": "Please create a task for the OAuth runbook and assign an owner.",
                "ts": "1735720512.000001",
                "thread_ts": "1735720511.000001"
            }),
        ],
    )
    .await
    .expect("start slack producer fixture");
    let _env_slack_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb_test_slack_fixture");
    let _env_slack_base = TempEnvVar::set(
        "SLACK_API_BASE_URL",
        &mock_api_base_url(&mock_server.base_url),
    );
    let _env_slack_user = TempEnvVar::remove("SLACK_USER_TOKEN");

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (agent, built_dir) =
        setup_semantic_ingress_agent_unlocked(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let registry = Arc::new(
        DispatchRegistry::new(
            "semantic-ingress-agent",
            "default",
            "semantic-ingress-agent",
            "1.0.0",
            agent,
        )
        .with_subscriptions(vec![EventSubscription {
            schema_versions: vec![EventSchemaVersion::parse("host.source-records.v1").unwrap()],
            source_kinds: vec![EventSourceKind::parse("slack").unwrap()],
            ..EventSubscription::default()
        }]),
    ) as Arc<dyn AgentRegistry>;

    let config_resolver = Arc::new(StaticConfigResolver {
        configs: HashMap::from([(
            SLACK_BUNDLE_NAME.to_string(),
            json!({ "channels": ["C123ABC456"] }),
        )]),
    }) as Arc<dyn ConfigResolver>;

    let producers = load_configured_event_producers_with_checkpoints(
        &InventoryCatalog::new(),
        Some(config_resolver),
        HashMap::new(),
    )
    .await
    .expect("load configured event producers");
    assert!(
        !producers.is_empty(),
        "expected at least one configured event producer; bundle {SLACK_BUNDLE_NAME} may no longer match the Slack provider registration"
    );

    let mut dispatcher = EventDispatcher::new(registry);
    for producer in producers {
        dispatcher
            .register_producer(producer)
            .expect("register producer");
    }

    let results = dispatcher.poll_and_deliver().await;
    let calls = handler.snapshot_calls().await;
    let hits = mock_state.snapshot_hits().await;

    assert_eq!(results.len(), 1, "expected one producer result");
    let (producer_key, outcome) = &results[0];
    assert_eq!(producer_key, "support/slack:id:C123ABC456");
    let outcome = outcome
        .as_ref()
        .expect("slack producer delivery should succeed");
    assert_eq!(outcome.subscribers_matched, 1);
    assert_eq!(outcome.subscribers_accepted, 1);
    assert!(
        outcome.failures.is_empty(),
        "expected no semantic-ingress delivery failures: {outcome:?}"
    );

    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation from semantic ingress; calls={calls:?}"
    );
    assert_eq!(calls[0].agent_package, "clickup-agent");
    assert!(
        calls[0]
            .prompt
            .contains("Ingress kind: Slack semantic ingress from raw source records"),
        "expected semantic-ingress prompt header, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0]
            .prompt
            .contains("Please create a task for the OAuth runbook and assign an owner."),
        "expected expanded thread transcript in delegated prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        hits.iter()
            .any(|hit| hit.contains("/api/conversations.history")),
        "expected producer poll to hit conversations.history, hits={hits:?}"
    );
    assert!(
        hits.iter()
            .any(|hit| hit.contains("/api/conversations.replies")),
        "expected semantic ingress to expand thread replies, hits={hits:?}"
    );

    mock_server.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_rejects_multi_message_batches() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");

    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [
            raw_slack_source_event("#agentium-eng", vec![]),
            raw_slack_source_event("#agentium-eng", vec![])
        ]
    });

    let response = timeout(
        Duration::from_secs(30),
        client.post(&dispatch_url).json(&dispatch_body).send(),
    )
    .await
    .expect("semantic-ingress multi-message dispatch timed out")
    .expect("semantic-ingress multi-message dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("semantic-ingress multi-message dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("expected exactly one dispatch message"),
        "expected multi-message rejection detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "unexpected downstream delegation for rejected multi-message dispatch"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn semantic_ingress_dispatch_http_rejects_noncanonical_raw_source_routing_key() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_semantic_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("semantic-ingress-agent", agent)
        .await
        .expect("start semantic-ingress runner api");

    let client = reqwest::Client::new();
    let dispatch_url = semantic_ingress_dispatch_url(&runner_api.base_url);
    let dispatch_body = json!({
        "routing_key": "slack:intake",
        "message_type": "host.source-records.v1",
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
