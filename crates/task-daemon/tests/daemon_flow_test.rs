use std::{
    net::SocketAddr,
    sync::{Arc, OnceLock},
};

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    routing::get,
};
use baml_task_daemon::{
    ExtractionMode, ProjectContext, SlackChannelSelector, SlackSourceConfig, SlackTaskSource,
    StateStore, TaskBatch, TaskDaemon, TaskExtractor, TaskSink,
};
use integrations_slack_read::SlackAuthPreference;
use reqwest::Url;
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
struct MockSlackState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl MockSlackState {
    async fn push_hit(&self, hit: String) {
        self.hits.lock().await.push(hit);
    }

    async fn snapshot_hits(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }
}

#[derive(Clone, Default)]
struct CaptureSink {
    batches: Arc<tokio::sync::Mutex<Vec<TaskBatch>>>,
}

impl CaptureSink {
    async fn snapshot(&self) -> Vec<TaskBatch> {
        self.batches.lock().await.clone()
    }
}

#[async_trait]
impl TaskSink for CaptureSink {
    fn name(&self) -> &'static str {
        "capture"
    }

    async fn deliver(&mut self, batch: &TaskBatch) -> Result<()> {
        self.batches.lock().await.push(batch.clone());
        Ok(())
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

async fn start_slack_mock_server() -> std::io::Result<(RunningServer, MockSlackState)> {
    async fn conversations_list(
        State(state): State<MockSlackState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state.push_hit(format!("GET {}", uri.0)).await;
        Json(json!({
            "ok": true,
            "channels": [
                {
                    "id": "CAGENTIUM1",
                    "name": "agentium-eng"
                }
            ],
            "response_metadata": {
                "next_cursor": ""
            }
        }))
    }

    async fn conversations_history(
        State(state): State<MockSlackState>,
        uri: OriginalUri,
    ) -> Json<Value> {
        state.push_hit(format!("GET {}", uri.0)).await;
        Json(json!({
            "ok": true,
            "messages": [
                {
                    "type": "message",
                    "user": "UALICE",
                    "username": "Alice",
                    "text": "TODO: investigate Slack daemon reliability",
                    "ts": "1735689600.000000"
                },
                {
                    "type": "message",
                    "user": "UBOB",
                    "username": "Bob",
                    "text": "Should we change delivery semantics?",
                    "ts": "1735689700.000000"
                }
            ],
            "has_more": false,
            "response_metadata": {
                "next_cursor": ""
            }
        }))
    }

    let state = MockSlackState::default();
    let app = Router::new()
        .route("/api/conversations.list", get(conversations_list))
        .route("/api/conversations.history", get(conversations_history))
        .with_state(state.clone());
    let server = start_http_server(app).await?;
    Ok((server, state))
}

#[tokio::test]
async fn slack_poll_interprets_discussion_and_persists_cursor() {
    let _gate = env_serial_gate().lock().await;

    let (server, mock_state) = start_slack_mock_server().await.expect("start Slack mock");
    let _env_slack_token = TempEnvVar::set("SLACK_BOT_TOKEN", "xoxb-test");
    let _env_slack_base =
        TempEnvVar::set("SLACK_API_BASE_URL", &format!("{}/api", server.base_url));
    let _env_slack_workspace = TempEnvVar::set("SLACK_WORKSPACE_URL", "https://acme.slack.com");

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_path = temp_dir.path().join("task-daemon-state.json");

    let source = SlackTaskSource::new(SlackSourceConfig {
        channel: SlackChannelSelector::parse("#agentium-eng").expect("selector"),
        history_limit: 200,
        max_pages: 1,
        auth_preference: SlackAuthPreference::Auto,
        initial_lookback_seconds: 86_400,
        workspace_url: Some(Url::parse("https://acme.slack.com").expect("workspace URL")),
    });

    let sink = CaptureSink::default();
    let sink_handle = sink.clone();

    let mut daemon = TaskDaemon::new(
        Box::new(source),
        TaskExtractor::with_mode(20, ExtractionMode::Heuristic).expect("extractor"),
        vec![Box::new(sink)],
        StateStore::new(state_path.clone(), 5_000),
        ProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: true,
            repo_path: Some("/repo/agent-platform".to_string()),
        },
    );

    let first = daemon.run_once().await.expect("first poll should succeed");
    assert_eq!(first.source_label, "#agentium-eng");
    assert!(!first.derived_tasks.is_empty());
    assert!(
        !first
            .interpretation
            .workflow_seed
            .investigation_nodes
            .is_empty()
    );

    let second = daemon.run_once().await.expect("second poll should succeed");
    assert!(second.derived_tasks.is_empty());

    let captured = sink_handle.snapshot().await;
    assert_eq!(
        captured.len(),
        1,
        "empty second batch should not be delivered"
    );

    let hits = mock_state.snapshot_hits().await;
    assert!(
        hits.iter().any(|hit| hit.contains("conversations.history")),
        "expected history API call"
    );

    let persisted = std::fs::read_to_string(state_path).expect("read state file");
    assert!(persisted.contains("1735689700.000000"));

    server.stop().await;
}
