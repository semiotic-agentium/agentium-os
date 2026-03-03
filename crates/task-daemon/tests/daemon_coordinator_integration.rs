use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::StatusCode,
    routing::post,
};
use baml_task_daemon::{
    A2aSink, InvestigationTask, ProjectContext, ProjectInterpretation, SourceReference, TaskBatch,
    TaskConfidence, TaskSink, TaskSourceKind,
};
use serde_json::{Value, json};

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
struct CoordinatorMockState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
    requests: Arc<tokio::sync::Mutex<Vec<Value>>>,
}

impl CoordinatorMockState {
    async fn push_hit(&self, hit: String) {
        self.hits.lock().await.push(hit);
    }

    async fn push_request(&self, request: Value) {
        self.requests.lock().await.push(request);
    }

    async fn snapshot_hits(&self) -> Vec<String> {
        self.hits.lock().await.clone()
    }

    async fn snapshot_requests(&self) -> Vec<Value> {
        self.requests.lock().await.clone()
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

fn sample_batch() -> TaskBatch {
    TaskBatch {
        source: TaskSourceKind::Slack,
        source_label: "#agentium-eng".to_string(),
        generated_at_unix: 1_735_720_000,
        messages_scanned: 3,
        project: ProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: true,
            repo_path: Some("/repo/agent-platform".to_string()),
        },
        interpretation: ProjectInterpretation {
            executive_summary: "Team agreed to investigate task-daemon sink reliability"
                .to_string(),
            ..ProjectInterpretation::default()
        },
        derived_tasks: vec![InvestigationTask {
            key: "prompt-1".to_string(),
            title: "Investigate coordinator delegation payload".to_string(),
            description: "Confirm typed handoff arrives as structured data".to_string(),
            priority: TaskConfidence::High,
            sources: vec![SourceReference {
                reference: "slack://channel/C123/p1735720000000000".to_string(),
                permalink: Some(
                    "https://acme.slack.com/archives/C123/p1735720000000000".to_string(),
                ),
                channel_id: Some("C123".to_string()),
                message_ts: Some("1735720000.000000".to_string()),
                thread_ts: None,
            }],
        }],
    }
}

#[tokio::test]
async fn a2a_sink_sends_typed_handoff_to_coordinator_endpoint() {
    async fn a2a_handler(
        State(state): State<CoordinatorMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        Json(json!([
            {"jsonrpc":"2.0","id":"1","result":{"final":false}},
            {
                "jsonrpc":"2.0",
                "id":"1",
                "result":{
                    "final":true,
                    "message":{"parts":[{"text":"Created tasks successfully"}]}
                }
            }
        ]))
    }

    let state = CoordinatorMockState::default();
    let app = Router::new()
        .route("/agents/coordinator-agent/default/a2a", post(a2a_handler))
        .with_state(state.clone());
    let server = start_http_server(app)
        .await
        .expect("start mock coordinator");

    let mut sink = A2aSink::new(server.base_url.clone(), false).expect("a2a sink");
    sink.deliver(&sample_batch())
        .await
        .expect("deliver to mock coordinator");

    let hits = state.snapshot_hits().await;
    assert!(
        hits.iter()
            .any(|hit| hit.contains("/agents/coordinator-agent/default/a2a")),
        "expected /a2a endpoint to be called, got {hits:?}"
    );

    let requests = state.snapshot_requests().await;
    assert_eq!(requests.len(), 1, "expected one A2A request");
    let request = &requests[0];

    assert_eq!(
        request.pointer("/method").and_then(Value::as_str),
        Some("message.sendStream")
    );
    assert!(
        request
            .pointer("/params/message/messageId")
            .and_then(Value::as_str)
            .is_some(),
        "request must include messageId"
    );
    assert_eq!(
        request
            .pointer("/params/message/role")
            .and_then(Value::as_str),
        Some("user")
    );
    assert_eq!(
        request
            .pointer("/params/message/parts/1/data/schema_version")
            .and_then(Value::as_str),
        Some("task-daemon.coordinator-handoff.v1")
    );
    assert_eq!(
        request
            .pointer("/params/message/parts/1/data/batch/project/project_key")
            .and_then(Value::as_str),
        Some("agent-platform")
    );

    server.stop().await;
}

#[tokio::test]
async fn a2a_sink_surfaces_non_success_status_with_body() {
    async fn failing_handler(
        State(state): State<CoordinatorMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> (StatusCode, String) {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        (
            StatusCode::BAD_REQUEST,
            "coordinator could not parse payload".to_string(),
        )
    }

    let state = CoordinatorMockState::default();
    let app = Router::new()
        .route(
            "/agents/coordinator-agent/default/a2a",
            post(failing_handler),
        )
        .with_state(state.clone());
    let server = start_http_server(app)
        .await
        .expect("start mock coordinator");

    let mut sink = A2aSink::new(server.base_url.clone(), false).expect("a2a sink");
    let err = sink
        .deliver(&sample_batch())
        .await
        .expect_err("deliver should fail on non-2xx");

    let msg = format!("{err:#}");
    assert!(msg.contains("coordinator A2A request failed"));
    assert!(msg.contains("400"));
    assert!(msg.contains("coordinator could not parse payload"));

    server.stop().await;
}
