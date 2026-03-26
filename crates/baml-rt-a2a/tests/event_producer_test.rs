//! End-to-end tests for the event-producer → dispatcher → agent pipeline.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use baml_rt_a2a::{A2aAgent, AgentRegistry, EventDispatcher};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentDispatchRoutingKey, AgentLister, AgentRouteKey, BamlRtError,
    BusStream, EventSchemaVersion, EventSourceKind, ProducedEvent, Result,
    bus::BusWithEffects,
    event_subscription::{EventSourceKey, EventSubscription},
};
use baml_rt_quickjs::BamlRuntimeManager;
use baml_rt_tools::{EventProducer, ProducerCheckpoint, ProducerPoll};
use serde_json::json;
use test_support::common::{
    TempDirCleanup, build_agent_package_to_temp, ensure_fixture_runtime_types, test_surreal_store,
    workspace_root,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fixture_agent_dir(name: &str) -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("agents")
        .join(name)
}

async fn setup_fixture_agent(name: &str) -> (A2aAgent, PathBuf) {
    let built = build_agent_package_to_temp(fixture_agent_dir(name), name).await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("fixture path utf8"))
        .expect("load fixture schema");

    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("fixture dist/index.js");
    let agent = A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(test_surreal_store().await)
        .build()
        .await
        .expect("build fixture agent");

    (agent, built)
}

// ---------------------------------------------------------------------------
// TestRegistry: wraps a single A2aAgent for dispatch + provides discovery
// ---------------------------------------------------------------------------

struct TestRegistry {
    agent: A2aAgent,
    entries: Vec<AgentDiscoveryEntry>,
}

impl AgentLister for TestRegistry {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        self.entries.clone()
    }
}

#[async_trait]
impl AgentRegistry for TestRegistry {
    async fn handle_a2a_stream(
        &self,
        _key: &AgentRouteKey,
        _request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        Err(BamlRtError::InvalidArgument(
            "a2a stream not supported in test registry".into(),
        ))
    }

    async fn handle_dispatch(
        &self,
        _key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck> {
        self.agent.handle_dispatch(request).await
    }
}

fn dispatch_echo_discovery_entry() -> AgentDiscoveryEntry {
    AgentDiscoveryEntry {
        agent_package: "dispatch-echo".into(),
        agent_instance_id: "default".into(),
        name: "dispatch-echo".into(),
        version: "1.0.0".into(),
        agent_card: AgentCard {
            name: "dispatch-echo".into(),
            version: "1.0.0".into(),
            content_hash: None,
            repository_version: None,
            agent_package: "dispatch-echo".into(),
            agent_instance_id: "default".into(),
            tools: vec![],
            baml_functions: vec![],
            description: Some("Fixture: echoes dispatch requests".into()),
            capabilities: vec!["dispatch:echo".into()],
            tags: vec![],
            subscriptions: vec![EventSubscription {
                schema_versions: vec![
                    EventSchemaVersion::parse("task-daemon.interpretation.v1").unwrap(),
                ],
                source_kinds: vec![
                    EventSourceKind::parse("slack").unwrap(),
                    EventSourceKind::parse("clickup").unwrap(),
                    EventSourceKind::parse("github_issues").unwrap(),
                ],
                ..EventSubscription::default()
            }],
        },
    }
}

// ---------------------------------------------------------------------------
// StubProducer
// ---------------------------------------------------------------------------

struct StubProducer {
    key: String,
    kinds: Vec<EventSourceKind>,
    events: Vec<ProducedEvent>,
    next_checkpoint: ProducerCheckpoint,
}

impl StubProducer {
    fn slack_event() -> ProducedEvent {
        ProducedEvent {
            routing_key: AgentDispatchRoutingKey::parse("slack:intake").unwrap(),
            schema_version: EventSchemaVersion::parse("task-daemon.interpretation.v1").unwrap(),
            source_kind: EventSourceKind::parse("slack").unwrap(),
            source_key: EventSourceKey::parse("slack:#test").unwrap(),
            messages: vec![json!({
                "schema_version": "task-daemon.interpretation.v1",
                "event_id": "test-producer-1",
                "source": { "source_key": "slack:#test", "source": "slack", "source_label": "#test" },
                "messages_scanned": 1,
                "derived_tasks": []
            })],
            context_id: None,
            task_id: None,
            message_id: Some("test-msg-001".into()),
            metadata: None,
        }
    }

    fn matching() -> Self {
        Self {
            key: "test:slack".into(),
            kinds: vec![EventSourceKind::parse("slack").unwrap()],
            events: vec![Self::slack_event()],
            next_checkpoint: ProducerCheckpoint::some("cursor-after-delivery"),
        }
    }

    /// Producer declares source_kinds=["clickup"] but emits a "slack" event.
    fn mismatched_source_kind() -> Self {
        Self {
            key: "test:mismatch".into(),
            kinds: vec![EventSourceKind::parse("clickup").unwrap()],
            events: vec![Self::slack_event()], // slack event, but declared clickup
            next_checkpoint: ProducerCheckpoint::some("should-not-advance"),
        }
    }

    fn empty() -> Self {
        Self {
            key: "test:empty".into(),
            kinds: vec![EventSourceKind::parse("slack").unwrap()],
            events: vec![],
            next_checkpoint: ProducerCheckpoint::none(),
        }
    }
}

#[async_trait]
impl EventProducer for StubProducer {
    fn producer_key(&self) -> &str {
        &self.key
    }

    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.kinds
    }

    async fn poll(&self, _checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        Ok(ProducerPoll {
            events: self.events.clone(),
            checkpoint: self.next_checkpoint.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_and_deliver_reaches_subscribed_agent() {
    ensure_fixture_runtime_types();
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let registry: Arc<dyn AgentRegistry> = Arc::new(TestRegistry {
        agent,
        entries: vec![dispatch_echo_discovery_entry()],
    });

    let mut dispatcher = EventDispatcher::new(registry);
    dispatcher
        .register_producer(Arc::new(StubProducer::matching()))
        .expect("register producer");

    let results = dispatcher.poll_and_deliver().await;
    assert_eq!(results.len(), 1, "expected one producer result");

    let (key, outcome) = &results[0];
    assert_eq!(key, "test:slack");
    let outcome = outcome.as_ref().expect("delivery should succeed");
    assert_eq!(outcome.subscribers_matched, 1);
    assert_eq!(outcome.subscribers_accepted, 1);
    assert!(outcome.failures.is_empty());
}

#[tokio::test]
async fn deliver_event_errors_when_no_subscribers() {
    ensure_fixture_runtime_types();
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let registry: Arc<dyn AgentRegistry> = Arc::new(TestRegistry {
        agent,
        entries: vec![dispatch_echo_discovery_entry()],
    });

    let dispatcher = EventDispatcher::new(registry);

    // Event with source_kind "unknown_source" — dispatch-echo only subscribes to slack/clickup/github_issues
    let event = ProducedEvent {
        routing_key: AgentDispatchRoutingKey::parse("unknown:intake").unwrap(),
        schema_version: EventSchemaVersion::parse("custom.v1").unwrap(),
        source_kind: EventSourceKind::parse("unknown_source").unwrap(),
        source_key: EventSourceKey::parse("unknown:key").unwrap(),
        messages: vec![json!({"test": true})],
        context_id: None,
        task_id: None,
        message_id: None,
        metadata: None,
    };

    let result = dispatcher.deliver_event(event).await;
    assert!(result.is_err(), "expected no-subscriber error");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("no subscribed agents"),
        "unexpected error: {err_msg}"
    );
}

/// Producer that records the checkpoint it receives on each poll, so tests can
/// directly assert on checkpoint state.
struct RecordingProducer {
    key: String,
    kinds: Vec<EventSourceKind>,
    events: Vec<ProducedEvent>,
    next_checkpoint: ProducerCheckpoint,
    received_checkpoints: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl EventProducer for RecordingProducer {
    fn producer_key(&self) -> &str {
        &self.key
    }
    fn source_kinds(&self) -> &[EventSourceKind] {
        &self.kinds
    }
    async fn poll(&self, checkpoint: &ProducerCheckpoint) -> Result<ProducerPoll> {
        self.received_checkpoints
            .lock()
            .unwrap()
            .push(checkpoint.value().map(String::from));
        Ok(ProducerPoll {
            events: self.events.clone(),
            checkpoint: self.next_checkpoint.clone(),
        })
    }
}

#[tokio::test]
async fn checkpoint_does_not_advance_when_no_subscribers() {
    ensure_fixture_runtime_types();
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let registry: Arc<dyn AgentRegistry> = Arc::new(TestRegistry {
        agent,
        entries: vec![dispatch_echo_discovery_entry()],
    });

    let received = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let producer = RecordingProducer {
        key: "test:unknown".into(),
        kinds: vec![EventSourceKind::parse("unknown_source").unwrap()],
        events: vec![ProducedEvent {
            routing_key: AgentDispatchRoutingKey::parse("unknown:intake").unwrap(),
            schema_version: EventSchemaVersion::parse("custom.v1").unwrap(),
            source_kind: EventSourceKind::parse("unknown_source").unwrap(),
            source_key: EventSourceKey::parse("unknown:key").unwrap(),
            messages: vec![json!({"test": true})],
            context_id: None,
            task_id: None,
            message_id: None,
            metadata: None,
        }],
        next_checkpoint: ProducerCheckpoint::some("should-not-advance"),
        received_checkpoints: Arc::clone(&received),
    };

    let mut dispatcher = EventDispatcher::new(registry);
    dispatcher
        .register_producer(Arc::new(producer))
        .expect("register producer");

    // First poll: delivery fails (no matching subscribers).
    let results = dispatcher.poll_and_deliver().await;
    assert!(results[0].1.is_err(), "expected no-subscriber error");

    // Second poll: checkpoint should still be None (not "should-not-advance").
    let results2 = dispatcher.poll_and_deliver().await;
    assert!(results2[0].1.is_err(), "second poll should also fail");

    let checkpoints = received.lock().unwrap();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(
        checkpoints[0], None,
        "first poll should receive initial (empty) checkpoint"
    );
    assert_eq!(
        checkpoints[1], None,
        "second poll should receive same empty checkpoint (not advanced)"
    );
}

#[tokio::test]
async fn empty_poll_returns_zero_outcome() {
    ensure_fixture_runtime_types();
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let registry: Arc<dyn AgentRegistry> = Arc::new(TestRegistry {
        agent,
        entries: vec![dispatch_echo_discovery_entry()],
    });

    let mut dispatcher = EventDispatcher::new(registry);
    dispatcher
        .register_producer(Arc::new(StubProducer::empty()))
        .expect("register producer");

    let results = dispatcher.poll_and_deliver().await;
    assert_eq!(results.len(), 1);

    let (key, outcome) = &results[0];
    assert_eq!(key, "test:empty");
    let outcome = outcome.as_ref().expect("empty poll should not error");
    assert_eq!(outcome.subscribers_matched, 0);
    assert_eq!(outcome.subscribers_accepted, 0);
}

#[tokio::test]
async fn source_kind_mismatch_is_a_hard_error() {
    ensure_fixture_runtime_types();
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let registry: Arc<dyn AgentRegistry> = Arc::new(TestRegistry {
        agent,
        entries: vec![dispatch_echo_discovery_entry()],
    });

    let mut dispatcher = EventDispatcher::new(registry);
    dispatcher
        .register_producer(Arc::new(StubProducer::mismatched_source_kind()))
        .expect("register producer");

    let results = dispatcher.poll_and_deliver().await;
    assert_eq!(results.len(), 1);

    let (key, result) = &results[0];
    assert_eq!(key, "test:mismatch");
    assert!(result.is_err(), "expected source kind mismatch error");

    let err_msg = result.as_ref().unwrap_err().to_string();
    assert!(
        err_msg.contains("source_kind=slack"),
        "error should name the actual source kind: {err_msg}"
    );
    assert!(
        err_msg.contains("clickup"),
        "error should name the declared kinds: {err_msg}"
    );
}

/// Proves that `slack.messages.v1` events (raw messages, not interpreted) deliver
/// to agents subscribing to `source_kinds: ["slack"]` without restricting
/// schema_version. This is the subscription shape an agent would use to receive
/// raw Slack events from the new `SlackEventProducer`.
#[tokio::test]
async fn slack_messages_v1_delivers_to_slack_subscriber() {
    ensure_fixture_runtime_types();
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    // Use a broader subscription (source_kind only, no schema_version constraint)
    // to match the slack.messages.v1 schema.
    let mut entry = dispatch_echo_discovery_entry();
    entry.agent_card.subscriptions = vec![EventSubscription {
        source_kinds: vec![EventSourceKind::parse("slack").unwrap()],
        ..EventSubscription::default()
    }];

    let registry: Arc<dyn AgentRegistry> = Arc::new(TestRegistry {
        agent,
        entries: vec![entry],
    });

    let dispatcher = EventDispatcher::new(registry);

    let event = ProducedEvent {
        routing_key: AgentDispatchRoutingKey::parse("slack:intake").unwrap(),
        schema_version: EventSchemaVersion::parse("slack.messages.v1").unwrap(),
        source_kind: EventSourceKind::parse("slack").unwrap(),
        source_key: EventSourceKey::parse("slack:C_test").unwrap(),
        messages: vec![json!({
            "channel_id": "C_test",
            "channel": "test-channel",
            "messages": [
                { "ts": "1735689600.000001", "user": "U123", "text": "hello from slack" }
            ]
        })],
        context_id: None,
        task_id: None,
        message_id: None,
        metadata: None,
    };

    let outcome = dispatcher
        .deliver_event(event)
        .await
        .expect("delivery should succeed");
    assert_eq!(outcome.subscribers_matched, 1);
    assert_eq!(outcome.subscribers_accepted, 1);
    assert!(outcome.failures.is_empty());
}
