// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::StatusCode,
    routing::{get, post},
};
use baml_task_daemon::{
    ContractSource, DispatchSink, InterpretationRequestEvent, InvestigationTask, ProjectContext,
    ProjectInterpretation, SinkDeliveryMode, SourceReference, TaskBatch, TaskConfidence,
    TaskDispatch, TaskSink, TaskSourceKind,
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
struct DispatchMockState {
    hits: Arc<tokio::sync::Mutex<Vec<String>>>,
    requests: Arc<tokio::sync::Mutex<Vec<Value>>>,
}

impl DispatchMockState {
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
    let (listener, addr) = test_support::common::bind_ephemeral_tokio("127.0.0.1").await?;
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
            title: "Investigate event delivery payload".to_string(),
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

fn sample_dispatch() -> TaskDispatch {
    let batch = sample_batch();
    let request = InterpretationRequestEvent::new(
        ContractSource::new(
            "slack:C123".to_string(),
            TaskSourceKind::Slack,
            batch.source_label.clone(),
        ),
        batch.project.clone(),
        Vec::new(),
        None,
    );
    TaskDispatch::from_batch(request, batch)
}

#[tokio::test]
async fn dispatch_sink_sends_typed_handoff_to_explicit_target_agent_endpoint() {
    async fn dispatch_handler(
        State(state): State<DispatchMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        Json(json!({"accepted": true, "detail": "Created tasks successfully"}))
    }

    let state = DispatchMockState::default();
    let app = Router::new()
        .route(
            "/agents/workflow-intake-agent/default/dispatch",
            post(dispatch_handler),
        )
        .with_state(state.clone());
    let server = start_http_server(app)
        .await
        .expect("start mock dispatch host");

    let mut sink = DispatchSink::for_agent(
        server.base_url.clone(),
        "workflow-intake-agent".to_string(),
        "default".to_string(),
        SinkDeliveryMode::Live,
    )
    .expect("dispatch sink");
    sink.deliver(&sample_dispatch())
        .await
        .expect("deliver to mock dispatch host");

    let hits = state.snapshot_hits().await;
    assert!(
        hits.iter()
            .any(|hit| hit.contains("/agents/workflow-intake-agent/default/dispatch")),
        "expected /dispatch endpoint to be called, got {hits:?}"
    );

    let requests = state.snapshot_requests().await;
    assert_eq!(requests.len(), 1, "expected one dispatch request");
    let request = &requests[0];

    assert_eq!(
        request.pointer("/routing_key").and_then(Value::as_str),
        Some("slack:intake")
    );
    assert!(
        request
            .pointer("/message_id")
            .and_then(Value::as_str)
            .is_some(),
        "request must include messageId"
    );
    assert_eq!(
        request.pointer("/message_type").and_then(Value::as_str),
        Some("task-daemon.interpretation.v1")
    );
    assert_eq!(
        request
            .pointer("/messages/0/schema_version")
            .and_then(Value::as_str),
        Some("task-daemon.interpretation.v1")
    );
    assert_eq!(
        request
            .pointer("/messages/0/project/project_key")
            .and_then(Value::as_str),
        Some("agent-platform")
    );

    server.stop().await;
}

#[tokio::test]
async fn dispatch_sink_discovers_matching_subscribers_and_delivers_to_them() {
    async fn list_agents_handler(State(state): State<DispatchMockState>) -> Json<Value> {
        state.push_hit("GET /agents".to_string()).await;
        Json(json!([
            {
                "agent_package": "semantic-ingress-agent",
                "agent_instance_id": "default",
                "name": "semantic-ingress-agent",
                "version": "1.0.0",
                "agent_card": {
                    "name": "semantic-ingress-agent",
                    "version": "1.0.0",
                    "agent_package": "semantic-ingress-agent",
                    "agent_instance_id": "default",
                    "tools": ["system/internal_a2a"],
                    "baml_functions": [],
                    "description": "Consumes task-daemon events",
                    "capabilities": ["slack:intake"],
                    "subscriptions": [{
                        "schema_versions": ["task-daemon.interpretation.v1"],
                        "source_kinds": ["slack"],
                        "source_keys": [],
                        "source_key_prefixes": []
                    }]
                }
            },
            {
                "agent_package": "non-matching-agent",
                "agent_instance_id": "default",
                "name": "non-matching-agent",
                "version": "1.0.0",
                "agent_card": {
                    "name": "non-matching-agent",
                    "version": "1.0.0",
                    "agent_package": "non-matching-agent",
                    "agent_instance_id": "default",
                    "tools": ["system/internal_a2a"],
                    "baml_functions": [],
                    "description": "Different subscription",
                    "capabilities": ["clickup:intake"],
                    "subscriptions": [{
                        "schema_versions": ["task-daemon.interpretation.v1"],
                        "source_kinds": ["clickup"],
                        "source_keys": [],
                        "source_key_prefixes": []
                    }]
                }
            }
        ]))
    }

    async fn dispatch_handler(
        State(state): State<DispatchMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        Json(json!({"accepted": true, "detail": "Subscriber handled event"}))
    }

    let state = DispatchMockState::default();
    let app = Router::new()
        .route("/agents", get(list_agents_handler))
        .route(
            "/agents/semantic-ingress-agent/default/dispatch",
            post(dispatch_handler),
        )
        .with_state(state.clone());
    let server = start_http_server(app).await.expect("start mock host");

    let mut sink =
        DispatchSink::new(server.base_url.clone(), SinkDeliveryMode::Live).expect("dispatch sink");
    sink.deliver(&sample_dispatch())
        .await
        .expect("deliver to subscribed agent");

    let hits = state.snapshot_hits().await;
    assert!(hits.iter().any(|hit| hit == "GET /agents"));
    assert!(
        hits.iter()
            .any(|hit| hit.contains("/agents/semantic-ingress-agent/default/dispatch"))
    );
    assert!(
        !hits
            .iter()
            .any(|hit| hit.contains("/agents/non-matching-agent/default/dispatch"))
    );

    let requests = state.snapshot_requests().await;
    assert_eq!(
        requests.len(),
        1,
        "expected one matching subscriber delivery"
    );
    assert_eq!(
        requests[0]
            .pointer("/messages/0/schema_version")
            .and_then(Value::as_str),
        Some("task-daemon.interpretation.v1")
    );

    server.stop().await;
}

#[tokio::test]
async fn dispatch_sink_errors_when_no_subscriber_matches_published_event() {
    async fn list_agents_handler(State(state): State<DispatchMockState>) -> Json<Value> {
        state.push_hit("GET /agents".to_string()).await;
        Json(json!([
            {
                "agent_package": "clickup-only-agent",
                "agent_instance_id": "default",
                "name": "clickup-only-agent",
                "version": "1.0.0",
                "agent_card": {
                    "name": "clickup-only-agent",
                    "version": "1.0.0",
                    "agent_package": "clickup-only-agent",
                    "agent_instance_id": "default",
                    "tools": ["system/internal_a2a"],
                    "baml_functions": [],
                    "description": "Only wants clickup events",
                    "capabilities": [],
                    "subscriptions": [{
                        "schema_versions": ["task-daemon.interpretation.v1"],
                        "source_kinds": ["clickup"],
                        "source_keys": [],
                        "source_key_prefixes": []
                    }]
                }
            }
        ]))
    }

    let state = DispatchMockState::default();
    let app = Router::new()
        .route("/agents", get(list_agents_handler))
        .with_state(state.clone());
    let server = start_http_server(app).await.expect("start mock host");

    let mut sink =
        DispatchSink::new(server.base_url.clone(), SinkDeliveryMode::Live).expect("dispatch sink");
    let err = sink
        .deliver(&sample_dispatch())
        .await
        .expect_err("delivery should fail without matching subscriber");

    let msg = format!("{err:#}");
    assert!(msg.contains("no subscribed agents matched"));
    assert!(msg.contains("task-daemon.interpretation.v1"));
    assert!(msg.contains("slack"));

    server.stop().await;
}

#[tokio::test]
async fn dispatch_sink_reports_partial_subscriber_delivery_failures_with_success_context() {
    async fn list_agents_handler(State(state): State<DispatchMockState>) -> Json<Value> {
        state.push_hit("GET /agents".to_string()).await;
        Json(json!([
            {
                "agent_package": "semantic-ingress-agent",
                "agent_instance_id": "default",
                "name": "semantic-ingress-agent",
                "version": "1.0.0",
                "agent_card": {
                    "name": "semantic-ingress-agent",
                    "version": "1.0.0",
                    "agent_package": "semantic-ingress-agent",
                    "agent_instance_id": "default",
                    "tools": ["system/internal_a2a"],
                    "baml_functions": [],
                    "description": "Consumes task-daemon events",
                    "capabilities": ["slack:intake"],
                    "subscriptions": [{
                        "schema_versions": ["task-daemon.interpretation.v1"],
                        "source_kinds": ["slack"],
                        "source_keys": [],
                        "source_key_prefixes": []
                    }]
                }
            },
            {
                "agent_package": "audit-agent",
                "agent_instance_id": "default",
                "name": "audit-agent",
                "version": "1.0.0",
                "agent_card": {
                    "name": "audit-agent",
                    "version": "1.0.0",
                    "agent_package": "audit-agent",
                    "agent_instance_id": "default",
                    "tools": ["system/internal_a2a"],
                    "baml_functions": [],
                    "description": "Also consumes task-daemon events",
                    "capabilities": ["slack:intake"],
                    "subscriptions": [{
                        "schema_versions": ["task-daemon.interpretation.v1"],
                        "source_kinds": ["slack"],
                        "source_keys": [],
                        "source_key_prefixes": []
                    }]
                }
            }
        ]))
    }

    async fn ok_handler(
        State(state): State<DispatchMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        Json(json!({"accepted": true, "detail": "Handled event"}))
    }

    async fn bad_handler(
        State(state): State<DispatchMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> (StatusCode, String) {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        (StatusCode::BAD_GATEWAY, "downstream failure".to_string())
    }

    let state = DispatchMockState::default();
    let app = Router::new()
        .route("/agents", get(list_agents_handler))
        .route(
            "/agents/semantic-ingress-agent/default/dispatch",
            post(ok_handler),
        )
        .route("/agents/audit-agent/default/dispatch", post(bad_handler))
        .with_state(state.clone());
    let server = start_http_server(app).await.expect("start mock host");

    let mut sink =
        DispatchSink::new(server.base_url.clone(), SinkDeliveryMode::Live).expect("dispatch sink");
    let err = sink
        .deliver(&sample_dispatch())
        .await
        .expect_err("one failing subscriber should surface as an error");

    let msg = format!("{err:#}");
    assert!(msg.contains("delivered to 1 of 2 subscribed agents"));
    assert!(msg.contains("semantic-ingress-agent/default"));
    assert!(msg.contains("audit-agent/default"));
    assert!(msg.contains("downstream failure"));

    server.stop().await;
}

#[tokio::test]
async fn dispatch_sink_surfaces_non_success_status_with_body() {
    async fn failing_handler(
        State(state): State<DispatchMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> (StatusCode, String) {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        (
            StatusCode::BAD_REQUEST,
            "target agent could not parse payload".to_string(),
        )
    }

    let state = DispatchMockState::default();
    let app = Router::new()
        .route(
            "/agents/coordinator-agent/default/dispatch",
            post(failing_handler),
        )
        .with_state(state.clone());
    let server = start_http_server(app)
        .await
        .expect("start mock dispatch host");

    let mut sink = DispatchSink::for_agent(
        server.base_url.clone(),
        "coordinator-agent".to_string(),
        "default".to_string(),
        SinkDeliveryMode::Live,
    )
    .expect("dispatch sink");
    let err = sink
        .deliver(&sample_dispatch())
        .await
        .expect_err("deliver should fail on non-2xx");

    let msg = format!("{err:#}");
    assert!(msg.contains("dispatch request failed"));
    assert!(msg.contains("400"));
    assert!(msg.contains("target agent could not parse payload"));

    server.stop().await;
}

#[tokio::test]
async fn dispatch_sink_rejects_negative_dispatch_ack_on_http_200() {
    async fn rejected_ack_handler(
        State(state): State<DispatchMockState>,
        uri: OriginalUri,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        state.push_hit(format!("POST {}", uri.0)).await;
        state.push_request(payload).await;
        Json(json!({"accepted": false, "detail": "invalid params"}))
    }

    let state = DispatchMockState::default();
    let app = Router::new()
        .route(
            "/agents/coordinator-agent/default/dispatch",
            post(rejected_ack_handler),
        )
        .with_state(state.clone());
    let server = start_http_server(app)
        .await
        .expect("start mock dispatch host");

    let mut sink = DispatchSink::for_agent(
        server.base_url.clone(),
        "coordinator-agent".to_string(),
        "default".to_string(),
        SinkDeliveryMode::Live,
    )
    .expect("dispatch sink");
    let err = sink
        .deliver(&sample_dispatch())
        .await
        .expect_err("deliver should fail when target agent rejects the dispatch");

    let msg = format!("{err:#}");
    assert!(msg.contains("rejected delivery"));
    assert!(msg.contains("invalid params"));

    server.stop().await;
}
