#![cfg(all(feature = "llm-tests", feature = "clickup"))]

mod common;

use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{
    AgentType, GraphqliteProvenanceStore, GraphqliteStoreBuilder, ProvEvent,
    ProvenanceContextReader, ProvenanceConversationContextItem, ProvenanceWriter,
};
use baml_tools_clickup::ClickUpTool;
use common::{
    RunningHttpServer, TempDirCleanup, TempEnvVar, build_clickup_agent_to_temp_async, contains_kv,
    e2e_serial_gate, post_a2a_sse_collect, start_http_server, start_runner_api_server,
};
use serde_json::{Value, json};
use test_support::common::{chunks_from_responses, message_texts_from_chunks, send_stream_request};
use tokio::time::{Duration, sleep, timeout};

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
                    "description": "Fetch /contexts/{context_id}/mermaid while runtime is alive and assert sequence output.",
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

    let server = start_http_server(app).await?.with_base_path("/api/v2");

    Ok((server, state))
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

    let provenance = build_graphqlite_test_store();
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

    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("clickup-agent dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_graphqlite_store(provenance.clone())
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

async fn fetch_mermaid_context(base_url: &str, context_id: &ContextId) -> String {
    let http_client = reqwest::Client::new();
    let mermaid_url = format!("{base_url}/contexts/{}/mermaid", context_id.as_str());
    let mermaid_response = timeout(Duration::from_secs(20), http_client.get(mermaid_url).send())
        .await
        .expect("mermaid request timed out")
        .expect("mermaid request failed");
    assert!(
        mermaid_response.status().is_success(),
        "Expected 200 from /contexts/<context_id>/mermaid, got {}",
        mermaid_response.status()
    );
    mermaid_response.text().await.expect("mermaid body")
}

fn build_graphqlite_test_store() -> Arc<GraphqliteProvenanceStore> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "baml-rt-runner-clickup-{pid}-{unique}.db",
        pid = std::process::id(),
    ));
    GraphqliteStoreBuilder::file(path)
        .build()
        .expect("build isolated GraphQLite store")
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
    let _built_dir_guard = TempDirCleanup::new(built_dir);
    let runner_api = match start_runner_api_server(
        "clickup-agent",
        agent,
        provenance_reader.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_real_model_with_mock_server_and_mermaid_http: cannot bind runner API server: {err}"
            );
            return;
        }
    };

    let http_client = reqwest::Client::new();
    let a2a_url = format!(
        "{}/agents/clickup-agent/default/a2a/sse",
        runner_api.base_url
    );
    let context_id = ContextId::new(77, 7);
    let mut matched_tool_result: Option<Value> = None;
    let mut conversation_items: Vec<ProvenanceConversationContextItem> = Vec::new();
    let mut turn_texts: Vec<String> = Vec::new();
    let turn_prompts = [
        "How many tasks are in progress?",
        "Please continue and fetch the required ClickUp data to compute the exact count.",
        "Continue and use tool calls to finish the exact in-progress task count.",
        "If still pending, continue with the next required ClickUp tool call and complete the exact count.",
        "Continue from the same context and finish the exact in-progress count using ClickUp tool calls.",
    ];

    for (turn, prompt) in turn_prompts.iter().enumerate() {
        let correlation_id = baml_rt_core::correlation::generate_correlation_id();
        let request_body = send_stream_request(
            &format!("clickup-vox-{}", turn + 1),
            prompt,
            correlation_id.as_str(),
            Some(context_id.clone()),
        );

        let responses: Vec<Value> = timeout(
            Duration::from_secs(180),
            post_a2a_sse_collect(&http_client, &a2a_url, &request_body),
        )
        .await
        .expect("a2a SSE request timed out")
        .expect("a2a SSE request failed");
        assert!(
            !responses.is_empty(),
            "Expected non-empty JSON-RPC response array from /a2a/sse"
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
        for _ in 0..80 {
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
            if stagnant_polls >= 20 {
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
    let mermaid = fetch_mermaid_context(&runner_api.base_url, &context_id).await;
    assert!(
        mermaid.contains("sequenceDiagram"),
        "Expected Mermaid sequence diagram response, got: {mermaid}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
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
    let _built_dir_guard = TempDirCleanup::new(built_dir);
    let runner_api = match start_runner_api_server(
        "clickup-agent",
        agent,
        provenance_reader.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "Skipping test_e2e_clickup_get_task_description_fast: cannot bind runner API server: {err}"
            );
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
    let a2a_url = format!(
        "{}/agents/clickup-agent/default/a2a/sse",
        runner_api.base_url
    );
    let responses: Vec<Value> = timeout(
        Duration::from_secs(120),
        post_a2a_sse_collect(&http_client, &a2a_url, &request_body),
    )
    .await
    .expect("a2a SSE request timed out")
    .expect("a2a SSE request failed");
    assert!(
        !responses.is_empty(),
        "Expected non-empty JSON-RPC response array from /a2a/sse"
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
    let mermaid = fetch_mermaid_context(&runner_api.base_url, &context_id).await;
    assert!(
        mermaid.contains("sequenceDiagram"),
        "Expected Mermaid sequence diagram response, got: {mermaid}"
    );

    runner_api.stop().await;
    mock_server.stop().await;
}
