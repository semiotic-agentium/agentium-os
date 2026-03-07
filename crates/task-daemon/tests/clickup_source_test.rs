use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, OnceLock},
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::Uri,
    routing::get,
};
use baml_task_daemon::{ClickupSourceConfig, ClickupTaskSource, TaskDaemonState, TaskSource};
use serde_json::{Value, json};
use test_support::common::TempEnvVar;

fn env_serial_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

struct RunningServer {
    base_url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RunningServer {
    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[derive(Clone, Default)]
struct MockClickupState {
    lists: Arc<tokio::sync::Mutex<BTreeMap<String, Vec<Value>>>>,
    paged_responses: Arc<tokio::sync::Mutex<BTreeMap<String, BTreeMap<u32, Value>>>>,
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockClickupState {
    async fn set_list_tasks(&self, list_id: &str, tasks: Vec<Value>) {
        self.lists.lock().await.insert(list_id.to_string(), tasks);
    }

    async fn list_tasks(&self, list_id: &str) -> Vec<Value> {
        self.lists
            .lock()
            .await
            .get(list_id)
            .cloned()
            .unwrap_or_default()
    }

    async fn set_list_page_response(&self, list_id: &str, page: u32, response: Value) {
        self.paged_responses
            .lock()
            .await
            .entry(list_id.to_string())
            .or_default()
            .insert(page, response);
    }

    async fn page_response(&self, list_id: &str, page: u32) -> Option<Value> {
        self.paged_responses
            .lock()
            .await
            .get(list_id)
            .and_then(|responses| responses.get(&page).cloned())
    }

    async fn push_hit(&self, value: String) {
        self.hits.lock().await.push(value);
    }

    async fn hits(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

async fn start_http_server(app: Router) -> std::io::Result<RunningServer> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr: SocketAddr = listener.local_addr()?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = rx.await;
        });
        if let Err(err) = server.await {
            eprintln!("mock server exited with error: {err}");
        }
    });

    Ok(RunningServer {
        base_url: format!("http://{addr}"),
        shutdown: Some(tx),
    })
}

async fn start_clickup_mock_server() -> std::io::Result<(RunningServer, MockClickupState)> {
    async fn list_tasks(
        Path(list_id): Path<String>,
        uri: Uri,
        State(state): State<MockClickupState>,
    ) -> Json<Value> {
        state.push_hit(uri.to_string()).await;
        let page = uri
            .query()
            .and_then(|query| {
                query.split('&').find_map(|pair| {
                    let (key, value) = pair.split_once('=')?;
                    if key == "page" {
                        value.parse::<u32>().ok()
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(0);
        if let Some(response) = state.page_response(&list_id, page).await {
            return Json(response);
        }
        let tasks = state.list_tasks(&list_id).await;
        Json(json!({ "tasks": tasks }))
    }

    let state = MockClickupState::default();
    let app = Router::new()
        .route("/api/v2/list/{list_id}/task", get(list_tasks))
        .with_state(state.clone());
    let server = start_http_server(app).await?;
    Ok((server, state))
}

fn task_json(id: &str, name: &str, status: &str, url: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "status": { "status": status },
        "description": format!("Description for {name}"),
        "url": url,
        "priority": { "priority": "2" }
    })
}

#[tokio::test]
async fn clickup_source_emits_created_and_reconciliation_events() {
    let _gate = env_serial_gate().lock().await;

    let (server, mock_state) = start_clickup_mock_server()
        .await
        .expect("start ClickUp mock");
    let _env_clickup_token = TempEnvVar::set("CLICKUP_API_KEY", "pk-test");
    let _env_clickup_base = TempEnvVar::set(
        "CLICKUP_API_BASE_URL",
        &format!("{}/api/v2", server.base_url),
    );

    mock_state
        .set_list_tasks(
            "L123",
            vec![
                task_json("task-1", "Initial task", "open", "https://clickup/task-1"),
                task_json(
                    "task-2",
                    "Second task",
                    "in progress",
                    "https://clickup/task-2",
                ),
            ],
        )
        .await;

    let mut source = ClickupTaskSource::new(ClickupSourceConfig {
        list_ids: vec!["L123".to_string()],
    })
    .expect("clickup source");
    assert_eq!(source.source_key(), "clickup:L123");

    let mut state = TaskDaemonState::default();

    let first = source.poll(&mut state).await.expect("first poll");
    assert_eq!(first.source_label, "clickup:list:L123");
    assert_eq!(first.source_items_scanned, 2);
    assert_eq!(first.inferred_tasks().len(), 2);
    assert!(
        first
            .inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-created:task-1")
    );
    assert!(
        first
            .inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-created:task-2")
    );
    let hits = mock_state.hits().await;
    assert!(
        hits.iter()
            .any(|hit| hit.contains("include_closed=true") && hit.contains("page=0")),
        "ClickUp source should request include_closed and page query parameters"
    );

    let second = source.poll(&mut state).await.expect("second poll");
    assert!(
        second.inferred_tasks().is_empty(),
        "second poll should not re-emit created events"
    );

    mock_state
        .set_list_tasks(
            "L123",
            vec![
                task_json(
                    "task-1",
                    "Initial task",
                    "canceled",
                    "https://clickup/task-1",
                ),
                task_json(
                    "task-2",
                    "Second task",
                    "in progress",
                    "https://clickup/task-2",
                ),
            ],
        )
        .await;

    let third = source.poll(&mut state).await.expect("third poll");
    assert!(
        third
            .inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-terminal:task-1:canceled")
    );

    let third_repeat = source.poll(&mut state).await.expect("third repeat poll");
    assert!(
        third_repeat.inferred_tasks().is_empty(),
        "terminal transition should emit once per status transition"
    );

    mock_state
        .set_list_tasks(
            "L123",
            vec![task_json(
                "task-1",
                "Initial task",
                "canceled",
                "https://clickup/task-1",
            )],
        )
        .await;

    let fourth = source.poll(&mut state).await.expect("fourth poll");
    assert!(
        fourth
            .inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-removed:task-2")
    );

    mock_state
        .set_list_tasks(
            "L123",
            vec![
                task_json(
                    "task-1",
                    "Initial task",
                    "canceled",
                    "https://clickup/task-1",
                ),
                task_json("task-2", "Second task", "open", "https://clickup/task-2"),
            ],
        )
        .await;

    let fifth = source.poll(&mut state).await.expect("fifth poll");
    assert!(
        fifth
            .inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-created:task-2:r2")
    );

    mock_state
        .set_list_tasks(
            "L123",
            vec![task_json(
                "task-1",
                "Initial task",
                "canceled",
                "https://clickup/task-1",
            )],
        )
        .await;

    let sixth = source.poll(&mut state).await.expect("sixth poll");
    assert!(
        sixth
            .inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-removed:task-2:r2")
    );

    server.stop().await;
}

#[tokio::test]
async fn clickup_source_continues_after_fully_malformed_page() {
    let _gate = env_serial_gate().lock().await;

    let (server, mock_state) = start_clickup_mock_server()
        .await
        .expect("start ClickUp mock");
    let _env_clickup_token = TempEnvVar::set("CLICKUP_API_KEY", "pk-test");
    let _env_clickup_base = TempEnvVar::set(
        "CLICKUP_API_BASE_URL",
        &format!("{}/api/v2", server.base_url),
    );

    mock_state
        .set_list_page_response(
            "L999",
            0,
            json!({
                "tasks": [
                    { "name": "missing id" },
                    { "id": 7, "name": "wrong id type" }
                ],
                "last_page": false
            }),
        )
        .await;
    mock_state
        .set_list_page_response(
            "L999",
            1,
            json!({
                "tasks": [task_json("task-good", "Recovered task", "open", "https://clickup/task-good")],
                "last_page": true
            }),
        )
        .await;

    let mut source = ClickupTaskSource::new(ClickupSourceConfig {
        list_ids: vec!["L999".to_string()],
    })
    .expect("clickup source");
    let mut state = TaskDaemonState::default();

    let poll = source.poll(&mut state).await.expect("poll");
    assert!(
        poll.inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-created:task-good"),
        "expected valid tasks on a later page to be emitted"
    );

    let hits = mock_state.hits().await;
    assert!(
        hits.iter().any(|hit| hit.contains("page=1")),
        "expected source to request page=1 after malformed page=0"
    );

    server.stop().await;
}

#[tokio::test]
async fn clickup_source_continues_across_distinct_consecutive_malformed_pages() {
    let _gate = env_serial_gate().lock().await;

    let (server, mock_state) = start_clickup_mock_server()
        .await
        .expect("start ClickUp mock");
    let _env_clickup_token = TempEnvVar::set("CLICKUP_API_KEY", "pk-test");
    let _env_clickup_base = TempEnvVar::set(
        "CLICKUP_API_BASE_URL",
        &format!("{}/api/v2", server.base_url),
    );

    mock_state
        .set_list_page_response(
            "L998",
            0,
            json!({
                "tasks": [{ "name": "missing id page0" }],
                "last_page": false
            }),
        )
        .await;
    mock_state
        .set_list_page_response(
            "L998",
            1,
            json!({
                "tasks": [{ "id": 7, "name": "wrong id type page1" }],
                "last_page": false
            }),
        )
        .await;
    mock_state
        .set_list_page_response(
            "L998",
            2,
            json!({
                "tasks": [task_json("task-late", "Late valid task", "open", "https://clickup/task-late")],
                "last_page": true
            }),
        )
        .await;

    let mut source = ClickupTaskSource::new(ClickupSourceConfig {
        list_ids: vec!["L998".to_string()],
    })
    .expect("clickup source");
    let mut state = TaskDaemonState::default();

    let poll = source.poll(&mut state).await.expect("poll");
    assert!(
        poll.inferred_tasks()
            .iter()
            .any(|task| task.key == "clickup-created:task-late"),
        "expected valid tasks on later pages after consecutive malformed pages"
    );

    let hits = mock_state.hits().await;
    assert!(
        hits.iter().any(|hit| hit.contains("page=2")),
        "expected source to request page=2 after two malformed pages"
    );

    server.stop().await;
}
