// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "llm-tests", feature = "clickup"))]

mod common;

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt::baml::BamlRuntimeManager;
use baml_rt_core::{
    bus::BusWithEffects,
    ids::{AgentId, UuidId},
};
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, SurrealProvenanceStore};
use baml_tools_clickup::{
    CLICKUP_LIFECYCLE_EVENT_KIND, ClickUpTool, ClickupLifecycleEventRecord, ClickupProjectContext,
    batch_from_lifecycle_events, clickup_task_snapshot_value,
};
use common::{
    RunningHttpServer, TempDirCleanup, build_clickup_agent_to_temp_async, e2e_secs_ci_or_local,
    e2e_serial_gate, quickjs_config_with_host_ingress, start_http_server, start_runner_api_server,
    try_load_dotenv_for_tests,
};
use serde_json::{Value, json};
use test_support::common::{
    TempEnvVar, require_api_key, test_surreal_store, workspace_fnox_path, workspace_root,
};
use tokio::time::{Duration, timeout};

#[derive(Clone, Default)]
struct MockClickUpState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockClickUpState {
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
        state.hits.lock().await.push("GET /api/v2/team".to_string());
        Json(json!({ "teams": [{ "id": "9013491519", "name": "Acme Workspace" }] }))
    }

    async fn list_spaces(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(team_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET /api/v2/team/{team_id}/space"));
        Json(json!({ "spaces": [{ "id": "space-9001", "name": "Engineering" }] }))
    }

    async fn list_lists(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(space_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET /api/v2/space/{space_id}/list"));
        Json(json!({ "lists": [{ "id": "list-901325431486", "name": "Agent Platform" }] }))
    }

    async fn list_tasks(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(list_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET /api/v2/list/{list_id}/task"));
        Json(json!({
            "tasks": [{
                "id": "task-1",
                "name": "Investigate publish ingress",
                "status": { "status": "in progress" },
                "description": "Confirm host bus receives source records",
                "url": "https://app.clickup.com/t/task-1",
                "priority": { "priority": "high" }
            }]
        }))
    }

    async fn get_task(
        AxumState(state): AxumState<MockClickUpState>,
        AxumPath(task_id): AxumPath<String>,
    ) -> Json<Value> {
        state
            .hits
            .lock()
            .await
            .push(format!("GET /api/v2/task/{task_id}"));
        Json(json!({
            "id": task_id,
            "name": "Investigate publish ingress",
            "status": { "status": "in progress" },
            "description": "Confirm host bus receives source records",
            "url": "https://app.clickup.com/t/task-1",
            "priority": { "priority": "high" }
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

    let server = start_http_server(app, Some("/api/v2")).await?;
    Ok((server, state))
}

async fn setup_clickup_ingress_agent(
    clickup_api_base_url: &str,
) -> (baml_rt::A2aAgent, Arc<SurrealProvenanceStore>, PathBuf) {
    let built = build_clickup_agent_to_temp_async().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("clickup built path utf8"))
        .expect("load clickup schema");
    manager
        .register_tool(
            ClickUpTool::with_base_url(clickup_api_base_url).expect("construct clickup tool"),
        )
        .await
        .expect("register clickup tool");

    let provenance = test_surreal_store().await;
    let store = Arc::clone(&provenance);
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    provenance
        .add_event(ProvEvent::agent_booted(
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
        .with_quickjs_config(quickjs_config_with_host_ingress(Arc::clone(&store)))
        .with_surreal_store(store)
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build clickup agent");
    (agent, provenance, built)
}

fn clickup_ingress_dispatch_url(base_url: &str) -> String {
    format!(
        "{base}/agents/clickup-agent/default/dispatch",
        base = base_url.trim_end_matches('/')
    )
}

fn lifecycle_batch_message() -> Value {
    let batch = batch_from_lifecycle_events(
        "clickup:list:901325431486",
        "ClickUp list",
        Some(ClickupProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: true,
            repo_path: Some("/repo/agent-platform".to_string()),
        }),
        &[ClickupLifecycleEventRecord {
            record_kind: CLICKUP_LIFECYCLE_EVENT_KIND.to_string(),
            key: "clickup-created:task-1:1".to_string(),
            event: "created".to_string(),
            task_id: "task-1".to_string(),
            list_id: "901325431486".to_string(),
            revision: 1,
            snapshot: clickup_task_snapshot_value(
                "task-1",
                "901325431486",
                "Investigate publish ingress",
                "in progress",
                Some("Confirm host bus receives source records"),
                Some("https://app.clickup.com/t/task-1"),
                Some("high"),
            ),
            previous_snapshot: None,
        }],
        1_735_720_000,
    );
    serde_json::to_value(&batch).expect("serialize lifecycle batch")
}

#[tokio::test]
async fn clickup_source_ingress_dispatch_empty_batch_accepted_without_llm() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let (mock_server, _) = start_clickup_mock_server()
        .await
        .expect("start clickup mock");
    let (agent, _prov, built) = setup_clickup_ingress_agent(&mock_server.base_url).await;
    let _cleanup = TempDirCleanup::new(built);
    let runner_api = start_runner_api_server("clickup-agent", agent, test_surreal_store().await)
        .await
        .expect("runner api");
    let client = reqwest::Client::new();
    let empty_batch = batch_from_lifecycle_events("clickup:list:1", "list", None, &[], 0);
    let body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [serde_json::to_value(&empty_batch).expect("batch json")]
    });
    let response = client
        .post(clickup_ingress_dispatch_url(&runner_api.base_url))
        .json(&body)
        .send()
        .await
        .expect("dispatch request");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("ack json");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("No lifecycle records"),
        "expected noop detail, got: {ack:?}"
    );
}

#[tokio::test]
async fn clickup_source_ingress_dispatch_processes_lifecycle_batch() {
    try_load_dotenv_for_tests();
    let _api_key = require_api_key();

    // Mock ClickUp HTTP server does not validate the token; satisfy tool open resolution.
    let _clickup_key = TempEnvVar::set("CLICKUP_API_KEY", "pk_mock_clickup_ingress_e2e");

    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let (mock_server, mock_state) = start_clickup_mock_server()
        .await
        .expect("start clickup mock");
    let (agent, _prov, built) = setup_clickup_ingress_agent(&mock_server.base_url).await;
    let _cleanup = TempDirCleanup::new(built);

    let runner_api = start_runner_api_server("clickup-agent", agent, test_surreal_store().await)
        .await
        .expect("runner api");
    let client = reqwest::Client::new();
    let body = json!({
        "routing_key": "event:intake",
        "message_type": "host.source-records.v1",
        "messages": [lifecycle_batch_message()]
    });

    let dispatch_secs = e2e_secs_ci_or_local(300, 180);
    let response = timeout(
        Duration::from_secs(dispatch_secs),
        client
            .post(clickup_ingress_dispatch_url(&runner_api.base_url))
            .json(&body)
            .send(),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("dispatch timed out after {dispatch_secs}s");
    })
    .expect("dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("ack json");
    assert_eq!(
        ack.get("accepted").and_then(Value::as_bool),
        Some(true),
        "expected accepted dispatch ack, got: {ack:?}"
    );
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains("Processed ClickUp lifecycle ingress") && detail.contains("1 unit"),
        "expected per-unit ingress processing detail, got: {ack:?}"
    );
    assert!(
        !detail.contains("Routed create_pm_work"),
        "clickup-agent should process locally, not slack-style delegation: {detail}"
    );

    let hits = mock_state.snapshot().await;
    assert!(
        hits.iter().any(|h| h.contains("GET /api/v2/")),
        "expected ClickUp tool API calls during ingress processing, hits={hits:?}"
    );

    let manifest = fs::read_to_string(workspace_root().join("agents/clickup-agent/manifest.json"))
        .expect("read manifest");
    assert!(
        manifest.contains("host.source-records.v1") && manifest.contains("clickup"),
        "workspace clickup-agent manifest should declare clickup host subscription"
    );
}
