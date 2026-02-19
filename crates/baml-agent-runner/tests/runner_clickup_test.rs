#![cfg(all(feature = "llm-tests", feature = "clickup"))]

mod common;

use std::{fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use baml_rt::baml::BamlRuntimeManager;
use baml_rt_a2a::AgentRegistry;
use baml_rt_core::{
    A2aRequestHandler, AgentDiscoveryEntry, AgentRouteKey,
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphExporter, GraphqliteProvenanceStore, GraphqliteStoreBuilder, ProvEvent,
    ProvenanceContextReader, ProvenanceConversationContextItem, ProvenanceWriter,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
};
use baml_rt_tools::clickup::ClickUpTool;
use common::{
    StrictProvenanceWriter, TempEnvVar, build_clickup_agent_to_temp_async, e2e_serial_gate,
};
use serde_json::{Value, json};
use test_support::common::{chunks_from_responses, message_texts_from_chunks, send_stream_request};
use tokio::time::{Duration, sleep, timeout};

#[derive(Clone)]
struct SingleAgentRegistry {
    package: String,
    instance_id: String,
    name: String,
    version: String,
    agent: baml_rt::A2aAgent,
}

#[async_trait]
impl AgentRegistry for SingleAgentRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        vec![AgentDiscoveryEntry {
            agent_package: self.package.clone(),
            agent_instance_id: self.instance_id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
        }]
    }

    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: Value,
    ) -> baml_rt_core::Result<baml_rt_core::bus::BusStream<Value>> {
        if key.agent_package != self.package || key.agent_instance_id != self.instance_id {
            return Err(baml_rt_core::BamlRtError::InvalidArgument(format!(
                "Agent {}/{} not found",
                key.agent_package, key.agent_instance_id
            )));
        }
        self.agent.handle_a2a_stream(request).await
    }
}

struct TestMermaidService {
    store: Arc<GraphqliteProvenanceStore>,
}

impl TestMermaidService {
    fn new(store: Arc<GraphqliteProvenanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl baml_rt_api::MermaidService for TestMermaidService {
    async fn mermaid_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        let graph = exporter
            .export_by_context(context_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let simplified = simplify_graph(&graph);
        Ok(render_sequence_diagram(&simplified))
    }

    async fn mermaid_for_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        let graph = exporter
            .export_by_task(task_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let simplified = simplify_graph(&graph);
        Ok(render_sequence_diagram(&simplified))
    }
}

struct RunningHttpServer {
    base_url: String,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl RunningHttpServer {
    async fn stop(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.handle.await;
    }
}

#[derive(Clone, Default)]
struct MockClickUpState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockClickUpState {
    async fn push_hit(&self, entry: String) {
        self.hits.lock().await.push(entry);
    }

    async fn snapshot(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_clickup_mock_server() -> std::io::Result<(RunningHttpServer, MockClickUpState)> {
    use axum::{
        Json, Router,
        extract::{Path as AxumPath, State as AxumState},
        routing::get,
    };

    async fn list_teams(AxumState(state): AxumState<MockClickUpState>) -> Json<Value> {
        state.push_hit("GET /api/v2/team".to_string()).await;
        Json(json!({
            "teams": [
                { "id": "9013491519", "name": "Mock Workspace" }
            ]
        }))
    }

    async fn list_spaces(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(team_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET /api/v2/team/{team_id}/space"))
            .await;
        Json(json!({
            "spaces": [
                { "id": "space-9001", "name": "Engineering" }
            ]
        }))
    }

    async fn list_lists(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(space_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET /api/v2/space/{space_id}/list"))
            .await;
        Json(json!({
            "lists": [
                { "id": "list-901325431486", "name": "Agent Platform" }
            ]
        }))
    }

    async fn list_tasks(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(list_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .push_hit(format!("GET /api/v2/list/{list_id}/task"))
            .await;
        Json(json!({
            "tasks": [
                {
                    "id": "task-901",
                    "name": "Ship mock ClickUp E2E test",
                    "status": { "status": "in progress" },
                    "description": "Validate real-model execution against deterministic mock tool responses.",
                    "url": "https://app.clickup.com/t/task-901",
                    "assignees": [{ "username": "qa-bot" }],
                    "priority": { "priority": "high" },
                    "due_date": null
                },
                {
                    "id": "task-902",
                    "name": "Verify Mermaid export endpoint",
                    "status": { "status": "in progress" },
                    "description": "Fetch /mermaid/context while runtime is alive and assert sequence output.",
                    "url": "https://app.clickup.com/t/task-902",
                    "assignees": [{ "username": "platform-bot" }],
                    "priority": { "priority": "low" },
                    "due_date": null
                }
            ]
        }))
    }

    async fn get_task(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(task_id): AxumPath<String>,
    ) -> Json<Value> {
        state.push_hit(format!("GET /api/v2/task/{task_id}")).await;
        Json(json!({
            "id": task_id,
            "name": "Ship mock ClickUp E2E test",
            "status": { "status": "in progress" },
            "description": "Validate real-model execution against deterministic mock tool responses.",
            "url": "https://app.clickup.com/t/task-901",
            "assignees": [{ "username": "qa-bot" }],
            "priority": { "priority": "high" },
            "due_date": null
        }))
    }

    let state = MockClickUpState::default();
    let app = Router::new()
        .route("/api/v2/team", get(list_teams))
        .route("/api/v2/team/{team_id}/space", get(list_spaces))
        .route("/api/v2/space/{space_id}/list", get(list_lists))
        .route("/api/v2/list/{list_id}/task", get(list_tasks))
        .route("/api/v2/task/{task_id}", get(get_task))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok((
        RunningHttpServer {
            base_url: format!("http://{addr}/api/v2"),
            shutdown_tx,
            handle,
        },
        state,
    ))
}

async fn start_runner_api_server(
    agent: baml_rt::A2aAgent,
    provenance: Arc<GraphqliteProvenanceStore>,
) -> std::io::Result<RunningHttpServer> {
    let registry: Arc<dyn AgentRegistry> = Arc::new(SingleAgentRegistry {
        package: "clickup-agent".to_string(),
        instance_id: "default".to_string(),
        name: "clickup-agent".to_string(),
        version: "1.0.0".to_string(),
        agent,
    });
    let mermaid: Option<Arc<dyn baml_rt_api::MermaidService>> =
        Some(Arc::new(TestMermaidService::new(provenance)));

    let app = baml_rt_api::api_router(registry, mermaid, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(RunningHttpServer {
        base_url: format!("http://{addr}"),
        shutdown_tx,
        handle,
    })
}

async fn setup_clickup_agent_with_provenance()
-> (baml_rt::A2aAgent, Arc<GraphqliteProvenanceStore>, PathBuf) {
    let built = build_clickup_agent_to_temp_async().await;
    let mut manager = BamlRuntimeManager::new().expect("create manager");
    manager
        .load_schema(built.to_str().expect("clickup built path utf8"))
        .expect("load clickup schema");
    manager
        .register_tool(ClickUpTool::new())
        .await
        .expect("register clickup tool");

    let provenance = GraphqliteStoreBuilder::in_memory()
        .build()
        .expect("build GraphQLite store");
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
            ContextId::new(77, 1),
            agent_id.clone(),
            AgentType::new("clickup-agent").expect("agent type"),
            "1.0.0".to_string(),
            "clickup-agent@1.0.0".to_string(),
        ))
        .await
        .expect("write AgentBooted");

    let strict_writer = Arc::new(StrictProvenanceWriter::new(provenance.clone()));
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("clickup-agent dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_provenance_writer(strict_writer)
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build clickup agent");
    (agent, provenance, built)
}

fn maybe_task_status(status: &Value) -> Option<String> {
    status.as_str().map(ToOwned::to_owned).or_else(|| {
        status
            .get("status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn contains_kv(value: &Value, key: &str, expected: &str) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(k, v)| {
            (k == key && v.as_str() == Some(expected)) || contains_kv(v, key, expected)
        }),
        Value::Array(items) => items.iter().any(|v| contains_kv(v, key, expected)),
        _ => false,
    }
}

#[tokio::test]
async fn test_e2e_clickup_real_model_with_plan_discovery() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!(
            "Skipping test_e2e_clickup_real_model_with_mock_server_and_mermaid_http: OPENROUTER_API_KEY not set"
        );
        return;
    }

    let (mock_server, mock_state) = match start_clickup_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_real_model_with_mock_server_and_mermaid_http: cannot bind mock server: {err}"
            );
            return;
        }
    };
    let _env_clickup_api_key = TempEnvVar::set("CLICKUP_API_KEY", "pk_mock_clickup_for_test");
    let _env_clickup_base = TempEnvVar::set("CLICKUP_API_BASE_URL", &mock_server.base_url);

    let (agent, provenance_reader, built_dir) = setup_clickup_agent_with_provenance().await;
    let runner_api = match start_runner_api_server(agent, provenance_reader.clone()).await {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_real_model_with_mock_server_and_mermaid_http: cannot bind runner API server: {err}"
            );
            mock_server.stop().await;
            fs::remove_dir_all(&built_dir).ok();
            return;
        }
    };

    let http_client = reqwest::Client::new();
    let a2a_url = format!("{}/agents/clickup-agent/default/a2a", runner_api.base_url);
    let context_id = ContextId::new(77, 7);
    let mut matched_tool_result: Option<Value> = None;
    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut turn_texts: Vec<String> = Vec::new();
    let turn_prompts = [
        "How many tasks are in progress?",
        "Please continue and fetch the required ClickUp data to compute the exact count.",
        "Continue and use tool calls to finish the exact in-progress task count.",
    ];

    for (turn, prompt) in turn_prompts.iter().enumerate() {
        let correlation_id = baml_rt_core::correlation::generate_correlation_id();
        let request_body = send_stream_request(
            &format!("clickup-vox-{}", turn + 1),
            prompt,
            correlation_id.as_str(),
            Some(context_id.clone()),
        );

        let response = timeout(
            Duration::from_secs(180),
            http_client.post(&a2a_url).json(&request_body).send(),
        )
        .await
        .expect("a2a HTTP request timed out")
        .expect("a2a HTTP request failed");
        assert!(
            response.status().is_success(),
            "Expected 2xx from /a2a, got {}",
            response.status()
        );
        let responses: Vec<Value> = response
            .json()
            .await
            .expect("parse /a2a response body as JSON");
        assert!(
            !responses.is_empty(),
            "Expected non-empty JSON-RPC response array from /a2a"
        );

        let chunks = chunks_from_responses(&responses);
        let texts = message_texts_from_chunks(&chunks);
        assert!(
            !texts.is_empty(),
            "Expected at least one assistant message chunk. Raw: {}",
            serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
        );
        turn_texts.extend(texts);

        let mut last_signature = String::new();
        let mut stagnant_polls = 0u32;
        for _ in 0..40 {
            let items = provenance_reader
                .conversation_context(&context_id, Some(220))
                .await
                .unwrap_or_default();
            conversation_items = items.clone();
            matched_tool_result = items
                .iter()
                .filter(|item| item.source == "tool_result")
                .find_map(|item| {
                    let tasks = item
                        .content
                        .get("result")
                        .and_then(|result| result.get("tasks"))
                        .and_then(Value::as_array)?;
                    if tasks.len() == 2 {
                        Some(item.content.clone())
                    } else {
                        None
                    }
                });
            if matched_tool_result.is_some() {
                break;
            }
            let signature = serde_json::to_string(
                &conversation_items
                    .iter()
                    .map(|i| (&i.event_id, &i.source, &i.content))
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            if signature == last_signature {
                stagnant_polls += 1;
            } else {
                stagnant_polls = 0;
                last_signature = signature;
            }
            if stagnant_polls >= 12 {
                break;
            }
            sleep(Duration::from_millis(250)).await;
        }

        if matched_tool_result.is_some() {
            break;
        }
    }

    let tool_result = matched_tool_result.unwrap_or_else(|| {
        panic!(
            "Expected a tool_result with exactly 2 tasks in provenance context after follow-up turns. \
             Sources seen: {:?}. Assistant texts: {:?}",
            conversation_items
                .iter()
                .map(|i| i.source.as_str())
                .collect::<Vec<_>>(),
            turn_texts
        )
    });
    let tasks = tool_result
        .get("result")
        .and_then(|r| r.get("tasks"))
        .and_then(Value::as_array)
        .expect("tool_result.result.tasks array");
    assert_eq!(tasks.len(), 2, "mock should always return exactly 2 tasks");
    for task in tasks {
        let status = maybe_task_status(task.get("status").unwrap_or(&Value::Null))
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert_eq!(
            status, "in progress",
            "Expected task status 'in progress', got task={task:?}"
        );

        let description = task
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !description.trim().is_empty(),
            "Expected non-empty task description, task={task:?}"
        );

        let priority = task
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            matches!(priority.as_str(), "low" | "high"),
            "Expected task priority low/high, got task={task:?}"
        );
    }

    let has_clickup_tool_call = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("name"))
                .and_then(Value::as_str)
                == Some("support/clickup")
    });
    assert!(
        has_clickup_tool_call,
        "Expected at least one support/clickup tool_call item in provenance context"
    );

    let team_id_call_seen = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("args"))
                .map(|args| contains_kv(args, "team_id", "9013491519"))
                .unwrap_or(false)
    });
    assert!(
        team_id_call_seen,
        "Expected at least one tool call with team_id=9013491519"
    );

    let space_id_call_seen = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("args"))
                .map(|args| contains_kv(args, "space_id", "space-9001"))
                .unwrap_or(false)
    });
    assert!(
        space_id_call_seen,
        "Expected at least one tool call with space_id=space-9001"
    );

    let list_id_call_seen = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("args"))
                .map(|args| contains_kv(args, "list_id", "list-901325431486"))
                .unwrap_or(false)
    });
    assert!(
        list_id_call_seen,
        "Expected at least one list-tasks tool call with list_id=list-901325431486"
    );

    let mock_hits = mock_state.snapshot().await;
    assert!(
        mock_hits.iter().any(|hit| hit == "GET /api/v2/team"),
        "Expected mock ClickUp teams endpoint hit. hits={mock_hits:?}"
    );
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit == "GET /api/v2/team/9013491519/space"),
        "Expected mock ClickUp spaces endpoint hit. hits={mock_hits:?}"
    );
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit == "GET /api/v2/space/space-9001/list"),
        "Expected mock ClickUp lists endpoint hit. hits={mock_hits:?}"
    );
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit == "GET /api/v2/list/list-901325431486/task"),
        "Expected mock ClickUp list-task endpoint hit. hits={mock_hits:?}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
    fs::remove_dir_all(&built_dir).ok();
}

#[tokio::test]
async fn test_e2e_clickup_get_task_description_fast() {
    if std::env::var("BAML_SKIP_LLM_TESTS").is_ok() {
        eprintln!("Skipping LLM test: BAML_SKIP_LLM_TESTS set");
        return;
    }
    let _permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let _ = dotenvy::dotenv();
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!(
            "Skipping test_e2e_clickup_get_task_description_fast: OPENROUTER_API_KEY not set"
        );
        return;
    }

    let (mock_server, mock_state) = match start_clickup_mock_server().await {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_get_task_description_fast: cannot bind mock server: {err}"
            );
            return;
        }
    };
    let _env_clickup_api_key = TempEnvVar::set("CLICKUP_API_KEY", "pk_mock_clickup_for_test");
    let _env_clickup_base = TempEnvVar::set("CLICKUP_API_BASE_URL", &mock_server.base_url);

    let (agent, provenance_reader, built_dir) = setup_clickup_agent_with_provenance().await;
    let runner_api = match start_runner_api_server(agent, provenance_reader.clone()).await {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_get_task_description_fast: cannot bind runner API server: {err}"
            );
            mock_server.stop().await;
            fs::remove_dir_all(&built_dir).ok();
            return;
        }
    };

    let context_id = ContextId::new(77, 8);
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request_body = send_stream_request(
        "clickup-vox-gettask-1",
        "Get the description for task_id=task-901",
        correlation_id.as_str(),
        Some(context_id.clone()),
    );

    let http_client = reqwest::Client::new();
    let a2a_url = format!("{}/agents/clickup-agent/default/a2a", runner_api.base_url);
    let response = timeout(
        Duration::from_secs(120),
        http_client.post(&a2a_url).json(&request_body).send(),
    )
    .await
    .expect("a2a HTTP request timed out")
    .expect("a2a HTTP request failed");
    assert!(
        response.status().is_success(),
        "Expected 2xx from /a2a, got {}",
        response.status()
    );
    let responses: Vec<Value> = response
        .json()
        .await
        .expect("parse /a2a response body as JSON");
    assert!(
        !responses.is_empty(),
        "Expected non-empty JSON-RPC response array from /a2a"
    );

    let chunks = chunks_from_responses(&responses);
    let texts = message_texts_from_chunks(&chunks);
    assert!(
        !texts.is_empty(),
        "Expected at least one assistant message chunk. Raw: {}",
        serde_json::to_string_pretty(&responses).unwrap_or_else(|_| "?".to_string())
    );

    let mut matched_tool_result: Option<Value> = None;
    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut last_signature = String::new();
    let mut stagnant_polls = 0u32;
    for _ in 0..80 {
        let items = provenance_reader
            .conversation_context(&context_id, Some(120))
            .await
            .unwrap_or_default();
        conversation_items = items.clone();
        matched_tool_result = items
            .iter()
            .filter(|item| item.source == "tool_result")
            .find_map(|item| {
                let tasks = item
                    .content
                    .get("result")
                    .and_then(|result| result.get("tasks"))
                    .and_then(Value::as_array)?;
                if tasks.len() == 1
                    && tasks[0].get("id").and_then(Value::as_str) == Some("task-901")
                    && tasks[0]
                        .get("description")
                        .and_then(Value::as_str)
                        .map(|d| !d.trim().is_empty())
                        .unwrap_or(false)
                {
                    Some(item.content.clone())
                } else {
                    None
                }
            });
        if matched_tool_result.is_some() {
            break;
        }
        let signature = serde_json::to_string(
            &conversation_items
                .iter()
                .map(|i| (&i.event_id, &i.source, &i.content))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        if signature == last_signature {
            stagnant_polls += 1;
        } else {
            stagnant_polls = 0;
            last_signature = signature;
        }
        if stagnant_polls >= 12 {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }

    let tool_result = matched_tool_result.unwrap_or_else(|| {
        panic!(
            "Expected a tool_result for task-901 with description. Sources seen: {:?}",
            conversation_items
                .iter()
                .map(|i| i.source.as_str())
                .collect::<Vec<_>>()
        )
    });
    let task = tool_result
        .get("result")
        .and_then(|r| r.get("tasks"))
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.first())
        .expect("tool_result.result.tasks[0]");
    assert_eq!(
        task.get("id").and_then(Value::as_str),
        Some("task-901"),
        "Expected task-901 in tool_result"
    );
    let description = task
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        description.contains("deterministic mock tool responses"),
        "Expected task description content from mock, got: {description}"
    );

    let task_id_call_seen = conversation_items.iter().any(|item| {
        item.source == "tool_call"
            && item
                .content
                .get("tool_call")
                .and_then(|tc| tc.get("args"))
                .map(|args| contains_kv(args, "task_id", "task-901"))
                .unwrap_or(false)
    });
    assert!(
        task_id_call_seen,
        "Expected at least one tool call with task_id=task-901"
    );

    let mock_hits = mock_state.snapshot().await;
    assert!(
        mock_hits
            .iter()
            .any(|hit| hit == "GET /api/v2/task/task-901"),
        "Expected mock ClickUp get-task endpoint hit. hits={mock_hits:?}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
    fs::remove_dir_all(&built_dir).ok();
}
