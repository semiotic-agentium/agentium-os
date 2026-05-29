// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "llm-tests", feature = "slack"))]

mod common;

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::{A2aRequestHandler, baml::BamlRuntimeManager};
use baml_rt_a2a::{AgentRegistry, EventDispatcher};
use baml_rt_core::{
    AgentLister, EventSchemaVersion, EventSourceKind,
    bus::BusWithEffects,
    event_subscription::{EventSourceKey, EventSubscription},
    ingress_store::{IngressId, IngressItem, IngressStore},
};
use baml_rt_tools::{
    BundleName, ConfigResolver, InventoryCatalog,
    ingress_store::test_support::install_memory_ingress_store,
    load_configured_event_producers_with_checkpoints,
};
use baml_tools_slack::SlackTool;
use baml_tools_system::SystemBundle;
use common::{
    CapturingA2aHandler, DispatchRegistry, RunningHttpServer, StaticAgentList,
    build_agent_dir_to_temp_async, discovery_entry, e2e_serial_gate,
    quickjs_config_with_host_ingress, start_http_server, try_load_dotenv_for_tests,
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

struct StaticConfigResolver {
    configs: HashMap<String, Value>,
}

#[async_trait]
impl ConfigResolver for StaticConfigResolver {
    async fn get_config(&self, bundle_name: &BundleName) -> baml_rt_core::Result<Option<Value>> {
        Ok(self.configs.get(bundle_name.as_str()).cloned())
    }
}

async fn build_workspace_slack_source_ingress_agent() -> PathBuf {
    let agent_dir = workspace_root().join("agents").join("slack-agent");
    build_agent_dir_to_temp_async(agent_dir, "slack-agent").await
}

async fn setup_slack_source_ingress_agent_unlocked(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (baml_rt::A2aAgent, PathBuf) {
    let built = build_workspace_slack_source_ingress_agent().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("slack-agent built path utf8"))
        .expect("load slack-agent schema");

    let allowlist = [
        "support/slack",
        // SystemBundle registers the callback tool; include it so the allowlist does not block it.
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

    let store = test_surreal_store().await;
    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("slack-agent dist");
    let agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_quickjs_config(quickjs_config_with_host_ingress(Arc::clone(&store)))
        .with_surreal_store(store)
        .build()
        .await
        .expect("build slack-agent agent");

    (agent, built)
}

async fn setup_slack_source_ingress_agent(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (
    tokio::sync::SemaphorePermit<'static>,
    baml_rt::A2aAgent,
    PathBuf,
) {
    let permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (agent, built) = setup_slack_source_ingress_agent_unlocked(agent_list, a2a_handler).await;
    (permit, agent, built)
}

async fn enqueue_slack_inbox_source_records(
    store: &baml_tools_slack::test_support::MemoryIngressStore,
    channel_id: &str,
    records: Vec<Value>,
    ingress_id: &str,
) {
    let source_key =
        EventSourceKey::parse(format!("slack:{channel_id}")).expect("valid slack source key");
    let payload = json!({
        "schema_version": "host.source-records.v1",
        "emitted_at_unix": 1_735_720_511u64,
        "source": {
            "source_kind": "slack",
            "source_key": source_key.as_str(),
            "source_label": channel_id
        },
        "records": records
    });
    store
        .enqueue(&IngressItem {
            ingress_id: IngressId::parse(ingress_id).expect("valid ingress id"),
            source_key,
            payload_json: serde_json::to_string(&payload).expect("serialize inbox payload"),
            enqueued_at_unix_ms: 1_735_720_511_000,
        })
        .await
        .expect("enqueue durable slack ingress item");
}

fn raw_slack_source_event(source_label: &str, records: Vec<Value>) -> Value {
    let source_key = match source_label {
        "#agentium-eng" => "slack:C012TEST001",
        _ => "slack:C012TEST999",
    };

    json!({
        "schema_version": "host.source-records.v1",
        "emitted_at_unix": 1_735_720_111u64,
        "source": {
            "source_kind": "slack",
            "source_key": source_key,
            "source_label": source_label
        },
        "records": records
    })
}

fn slack_source_ingress_dispatch_url(base_url: &str) -> String {
    format!(
        "{base}/agents/slack-agent/default/dispatch",
        base = base_url
    )
}

fn mock_api_base_url(base_url: &str) -> String {
    format!("{base}/api", base = base_url)
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
async fn slack_source_ingress_dispatch_http_routes_raw_slack_source_records_to_task_management_creation_capability()
 {
    try_load_dotenv_for_tests();
    let _api_key = test_support::common::require_api_key();

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_slack_source_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");
    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent dispatch timed out")
    .expect("slack-agent dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("slack-agent dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Slack conversation unit"),
        "expected per-unit ingress processing detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "slack-agent must not delegate to downstream agents during dispatch ingress"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn slack_source_ingress_dispatch_http_noops_raw_slack_chatter_without_action_cues() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_slack_source_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");
    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent noop dispatch timed out")
    .expect("slack-agent noop dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("slack-agent noop ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Slack conversation unit"),
        "expected per-unit ingress processing detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "expected no downstream delegation for chatter"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn slack_source_ingress_dispatch_http_batches_mixed_raw_slack_work_items_into_one_delegation()
{
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_slack_source_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");
    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent mixed dispatch timed out")
    .expect("slack-agent mixed dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("slack-agent mixed ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));

    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("2 Slack conversation unit"),
        "expected two conversation units processed, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "slack-agent must not delegate during dispatch ingress"
    );

    runner_api.stop().await;
}

#[tokio::test]
async fn slack_source_ingress_dispatch_http_expands_slack_threads_before_deriving_work() {
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
        setup_slack_source_ingress_agent_unlocked(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");
    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent thread-expansion dispatch timed out")
    .expect("slack-agent thread-expansion dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("slack-agent thread-expansion ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));

    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Slack conversation unit"),
        "expected unit ingress processing without pre-dispatch Slack API expansion, got: {ack:?}"
    );
    assert!(handler.snapshot_calls().await.is_empty());
    assert!(
        mock_state.snapshot_hits().await.is_empty(),
        "ingress must not call conversations.replies before agent withTask"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}

#[tokio::test]
async fn slack_source_ingress_dispatch_http_does_not_expand_thread_root_without_replies() {
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
        setup_slack_source_ingress_agent_unlocked(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");
    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent root-only dispatch timed out")
    .expect("slack-agent root-only dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("slack-agent root-only dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));

    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Slack conversation unit"),
        "expected unit ingress from batch records only, got: {ack:?}"
    );
    assert!(handler.snapshot_calls().await.is_empty());
    assert!(
        mock_state.snapshot_hits().await.is_empty(),
        "expected no Slack thread-replies fetch during dispatch ingress, hits={:?}",
        mock_state.snapshot_hits().await
    );

    runner_api.stop().await;
    mock_server.stop().await;
}

// REST channel polling E2E lives in task-daemon (`daemon_coordinator_integration` and friends).
// Runner registers only the durable inbox drain (`support/slack:inbox`).

#[tokio::test]
async fn slack_inbox_producer_delivers_durable_ingress_to_slack_agent_and_downstream_delegation() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (_store_guard, store) = install_memory_ingress_store();

    let (mock_server, _mock_state) = start_slack_producer_server(
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
        setup_slack_source_ingress_agent_unlocked(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let registry = Arc::new(
        DispatchRegistry::new("slack-agent", "default", "slack-agent", "1.0.0", agent)
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

    enqueue_slack_inbox_source_records(
        &store,
        "C123ABC456",
        vec![json!({
            "type": "message",
            "user": "U123",
            "text": "Can we track the OAuth docs follow-up?",
            "ts": "1735720511.000001",
            "thread_ts": "1735720511.000001",
            "reply_count": 1,
            "latest_reply": "1735720512.000001"
        })],
        "support/slack:inbox:slack-agent-e2e",
    )
    .await;
    assert_eq!(store.undelivered_count().await, 1);

    let producers = load_configured_event_producers_with_checkpoints(
        &InventoryCatalog::new(),
        Some(config_resolver),
        HashMap::new(),
        Some(store.clone() as Arc<dyn baml_rt_core::IngressStore>),
    )
    .await
    .expect("load configured event producers");
    assert_eq!(
        producers.len(),
        1,
        "runner should register only the Slack inbox drain when ingress store is installed"
    );
    assert_eq!(producers[0].producer_key(), "support/slack:inbox");

    let mut dispatcher = EventDispatcher::new(
        registry,
        baml_rt_core::HostPublishService::without_provenance(),
    );
    for producer in producers {
        dispatcher
            .register_producer(producer)
            .expect("register producer");
    }

    let first_results = dispatcher.poll_and_deliver().await;
    assert_eq!(first_results.len(), 1, "expected one inbox producer result");
    let (producer_key, inbox_outcome) = &first_results[0];
    assert_eq!(producer_key, "support/slack:inbox");
    let inbox_outcome = inbox_outcome
        .as_ref()
        .expect("inbox producer delivery should succeed");
    assert_eq!(inbox_outcome.subscribers_matched, 1);
    assert_eq!(inbox_outcome.subscribers_accepted, 1);
    assert!(inbox_outcome.failures.is_empty());

    let second_results = dispatcher.poll_and_deliver().await;
    let second_result_by_key = second_results
        .iter()
        .map(|(producer_key, outcome)| (producer_key.as_str(), outcome))
        .collect::<HashMap<_, _>>();
    let second_inbox_outcome = second_result_by_key
        .get("support/slack:inbox")
        .expect("inbox producer second-cycle result")
        .as_ref()
        .expect("second-cycle inbox poll should succeed");
    assert_eq!(second_inbox_outcome.subscribers_matched, 0);
    assert_eq!(store.undelivered_count().await, 0);

    assert!(
        handler.snapshot_calls().await.is_empty(),
        "inbox-driven dispatch ingress must not delegate to downstream agents"
    );

    mock_server.stop().await;
}

#[tokio::test]
async fn slack_source_ingress_dispatch_http_rejects_multi_message_batches() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_slack_source_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");

    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent multi-message dispatch timed out")
    .expect("slack-agent multi-message dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("slack-agent multi-message dispatch ack");
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
async fn slack_source_ingress_dispatch_http_rejects_noncanonical_raw_source_routing_key() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "clickup-agent",
            &[PROJECT_MANAGEMENT_CREATE_TASK_CAPABILITY],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_slack_source_ingress_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let runner_api = start_runner_api_server("slack-agent", agent)
        .await
        .expect("start slack-agent runner api");

    let client = reqwest::Client::new();
    let dispatch_url = slack_source_ingress_dispatch_url(&runner_api.base_url);
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
    .expect("slack-agent bad-routing dispatch timed out")
    .expect("slack-agent bad-routing dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response
        .json()
        .await
        .expect("slack-agent bad-routing dispatch ack");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("slack-agent expected routing_key event:intake"),
        "expected routing-key rejection detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "unexpected downstream delegation for rejected routing key"
    );

    runner_api.stop().await;
}
