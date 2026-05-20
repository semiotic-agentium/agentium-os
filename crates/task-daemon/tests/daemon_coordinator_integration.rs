//! Integration: task-daemon publishes `host.source-records.v1` to runner `/events/publish`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{Router, routing::post};
use baml_rt_core::{
    ProducedEvent,
    event_subscription::{EventSourceKey, EventSourceKind},
    host_wire::wire,
};
use baml_task_daemon::{
    ProjectContext, PublishSink, SinkDeliveryMode, SlackMessage, SourcePoll, SourceReference,
    TaskDaemon, TaskSink, TaskSource,
};
use serde_json::Value;
use tempfile::TempDir;

struct OncePollSource {
    poll: SourcePoll,
    done: AtomicUsize,
}

#[async_trait::async_trait]
impl TaskSource for OncePollSource {
    fn source_key(&self) -> String {
        "slack:C123".to_string()
    }

    async fn poll(
        &mut self,
        _state: &mut baml_task_daemon::TaskDaemonState,
    ) -> anyhow::Result<SourcePoll> {
        if self.done.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(self.poll.clone())
        } else {
            Ok(SourcePoll::slack(
                self.source_key(),
                "#test".to_string(),
                Vec::new(),
                0,
            ))
        }
    }
}

struct CapturingPublish {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl TaskSink for CapturingPublish {
    fn name(&self) -> &'static str {
        "capturing"
    }

    async fn deliver(&mut self, event: &baml_rt_core::ProducedEvent) -> anyhow::Result<()> {
        assert_eq!(event.schema_version.as_str(), wire::HOST_SOURCE_RECORDS_V1);
        assert!(event.context_id.is_some());
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn publish_sink_posts_to_events_publish() {
    let received = Arc::new(AtomicUsize::new(0));
    let received_clone = received.clone();
    let app = Router::new().route(
        "/events/publish",
        post({
            move |axum::Json(body): axum::Json<Value>| {
                let count = received_clone.clone();
                async move {
                    assert_eq!(
                        body.get("schema_version").and_then(|v| v.as_str()),
                        Some(wire::HOST_SOURCE_RECORDS_V1)
                    );
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "subscribers_matched": 1,
                        "subscribers_accepted": 1,
                        "failures": []
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let base = format!("http://{addr}");
    let mut sink = PublishSink::new(base, SinkDeliveryMode::Live).expect("publish sink");
    let event = ProducedEvent::host_source_records(
        EventSourceKind::parse("slack").expect("kind"),
        EventSourceKey::parse("slack:C123").expect("key"),
        serde_json::json!({"records": []}),
        Some("td-poll-batch-test".to_string()),
        None,
    )
    .expect("event");
    sink.deliver(&event).await.expect("deliver");
    assert_eq!(received.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_once_mints_stable_context_and_delivers() {
    let dir = TempDir::new().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let poll = SourcePoll::slack(
        "slack:C123".to_string(),
        "#agentium-eng".to_string(),
        vec![
            SlackMessage {
                channel_name: "#agentium-eng".to_string(),
                channel_id: "C123".to_string(),
                ts: "1735689600.000000".to_string(),
                text: "first".to_string(),
                user_id: Some("U1".to_string()),
                thread_ts: None,
                user_name: None,
                subtype: None,
                source: SourceReference {
                    reference: "slack:1735689600.000000".to_string(),
                    permalink: None,
                    channel_id: Some("C123".to_string()),
                    message_ts: Some("1735689600.000000".to_string()),
                    thread_ts: None,
                },
            },
            SlackMessage {
                channel_name: "#agentium-eng".to_string(),
                channel_id: "C123".to_string(),
                ts: "1735689700.000000".to_string(),
                text: "second".to_string(),
                user_id: Some("U2".to_string()),
                thread_ts: None,
                user_name: None,
                subtype: None,
                source: SourceReference {
                    reference: "slack:1735689700.000000".to_string(),
                    permalink: None,
                    channel_id: Some("C123".to_string()),
                    message_ts: Some("1735689700.000000".to_string()),
                    thread_ts: None,
                },
            },
        ],
        2,
    );
    let count = Arc::new(AtomicUsize::new(0));
    let source = Box::new(OncePollSource {
        poll: poll.clone(),
        done: AtomicUsize::new(0),
    });
    let sinks: Vec<Box<dyn TaskSink>> = vec![Box::new(CapturingPublish {
        count: count.clone(),
    })];
    let mut daemon = TaskDaemon::new(
        source,
        sinks,
        baml_task_daemon::StateStore::new(state_path, 100),
        ProjectContext {
            project_key: "agent-platform".to_string(),
            repo_available: false,
            repo_path: None,
        },
    );
    let event = daemon.run_once().await.expect("run_once");
    assert_eq!(event.schema_version.as_str(), wire::HOST_SOURCE_RECORDS_V1);
    assert_eq!(
        event.context_id.as_ref().map(|c| c.as_str()),
        Some("ctx-7548386120284784534-8799862099676914443")
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
