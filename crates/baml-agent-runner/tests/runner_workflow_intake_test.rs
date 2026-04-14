#![cfg(feature = "llm-tests")]

#[allow(dead_code, unused_imports)]
mod common;

use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};

use baml_rt::{A2aRequestHandler, baml::BamlRuntimeManager};
use baml_rt_a2a::AgentRegistry;
use baml_rt_core::{A2aStreamChunk, A2aWireRequest, AgentLister, bus::BusWithEffects};
use baml_tools_system::SystemBundle;
use common::{
    CapturingA2aHandler, DispatchRegistry, FailingA2aHandler, StaticAgentList, StreamingA2aHandler,
    discovery_entry, e2e_serial_gate, start_http_server,
};
use serde_json::{Value, json};
use test_support::common::{
    TempDirCleanup, build_agent_package_to_temp, chunks_from_responses, message_texts_from_chunks,
    test_surreal_store, workspace_fnox_path, workspace_root,
};

async fn build_workspace_workflow_intake_agent() -> PathBuf {
    build_agent_package_to_temp(workflow_intake_agent_dir(), "workflow-intake-agent").await
}

fn workflow_intake_agent_dir() -> PathBuf {
    workspace_root()
        .join("agents")
        .join("workflow-intake-agent")
}

async fn setup_workflow_intake_agent_unlocked(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (baml_rt::A2aAgent, PathBuf) {
    let built = build_workspace_workflow_intake_agent().await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create manager");
    manager
        .load_schema(built.to_str().expect("workflow-intake path utf8"))
        .expect("load workflow-intake schema");

    let allowlist: HashSet<String> = [
        "system/internal_a2a",
        "system/callback",
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

    let agent_code =
        fs::read_to_string(built.join("dist").join("index.js")).expect("workflow-intake dist");
    let agent = baml_rt::A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_surreal_store(test_surreal_store().await)
        .build()
        .await
        .expect("build workflow-intake agent");

    (agent, built)
}

async fn setup_workflow_intake_agent(
    agent_list: Arc<dyn AgentLister>,
    a2a_handler: Arc<dyn A2aRequestHandler>,
) -> (
    tokio::sync::SemaphorePermit<'static>,
    baml_rt::A2aAgent,
    PathBuf,
) {
    let permit = e2e_serial_gate().acquire().await.expect("acquire e2e gate");
    let (agent, built) = setup_workflow_intake_agent_unlocked(agent_list, a2a_handler).await;
    (permit, agent, built)
}

async fn collect_responses(
    agent: &baml_rt::A2aAgent,
    request: Value,
) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await?;
    let chunks = baml_rt_core::collect_a2a_stream(stream).await;
    Ok(chunks.into_iter().map(A2aStreamChunk::into_inner).collect())
}

fn task_daemon_request(data: Value, message_id: &str, correlation_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "message.sendStream",
        "id": correlation_id,
        "params": {
            "message": {
                "messageId": message_id,
                "role": "user",
                "parts": [
                    {
                        "data": data
                    }
                ]
            }
        }
    })
}

fn base_event(source_kind: &str, source_label: &str, derived_tasks: Vec<Value>) -> Value {
    json!({
        "schema_version": "task-daemon.interpretation.v1",
        "event_id": "td-interpret-result-test",
        "request_event_id": "td-interpret-request-test",
        "emitted_at_unix": 1_735_720_001u64,
        "source": {
            "source_key": format!("{source_kind}:{source_label}"),
            "source": source_kind,
            "source_label": source_label
        },
        "messages_scanned": 3,
        "project": {
            "project_key": "agent-platform",
            "repo_available": true,
            "repo_path": "/repo/agent-platform"
        },
        "interpretation": {
            "executive_summary": "Move daemon events into the right downstream workflow without duplicate work.",
            "current_objectives": [
                "Route work to the correct capability owner"
            ]
        },
        "derived_tasks": derived_tasks
    })
}

fn with_workflow_seed(mut event: Value, workflow_seed: Value) -> Value {
    event["interpretation"]["workflow_seed"] = workflow_seed;
    event
}

fn task_state(chunk: &Value) -> Option<&str> {
    chunk
        .get("task")
        .and_then(|task| task.get("status"))
        .and_then(|status| status.get("state"))
        .and_then(Value::as_str)
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|status_update| status_update.get("status"))
                .and_then(|status| status.get("state"))
                .and_then(Value::as_str)
        })
}

#[tokio::test]
async fn workflow_intake_rejects_slack_task_daemon_events() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry("clickup-agent", &["clickup:create-task"])],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) = setup_workflow_intake_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "slack",
            "#agentium-eng",
            vec![json!({
                "key": "task-1",
                "title": "Investigate duplicate task execution",
                "description": "Turn the Slack discussion into tracked PM work.",
                "priority": "high"
            })],
        ),
        "workflow-intake-slack-1",
        "corr-1735720000000-1",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let rendered = serde_json::to_string(&responses).expect("serialize responses");

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected unsupported Slack task-daemon event to fail, got: {responses:?}"
    );
    assert!(
        rendered.contains(
            "workflow-intake-agent no longer routes Slack task-daemon interpretation events"
        ),
        "expected unsupported-source error in response, got: {responses:?}"
    );
}

#[tokio::test]
async fn workflow_intake_dispatch_http_routes_clickup_noop_ack() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry("clickup-agent", &["clickup:create-task"])],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let registry = Arc::new(DispatchRegistry::new(
        "workflow-intake-agent",
        "default",
        "workflow-intake-agent",
        "1.0.0",
        agent,
    )) as Arc<dyn AgentRegistry>;
    let app = baml_rt_api::api_router(registry, None, None).await;
    let server = start_http_server(app, None)
        .await
        .expect("start dispatch http api");
    let base_url = server.base_url.clone();

    let client = reqwest::Client::new();
    let context_id = "ctx-1735720000000-42";
    let dispatch_url = format!("{}/agents/workflow-intake-agent/default/dispatch", base_url);
    let dispatch_body = json!({
        "routing_key": "clickup:intake",
        "message_type": "task-daemon.interpretation.v1",
        "context_id": context_id,
        "task_id": "dispatch-task-1735720000000",
        "message_id": "dispatch-msg-1735720000000",
        "messages": [
            base_event("clickup", "ClickUp monitored list", vec![])
        ]
    });

    let response = client
        .post(&dispatch_url)
        .json(&dispatch_body)
        .send()
        .await
        .expect("dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("dispatch ack json");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(true));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.to_ascii_lowercase().contains("no derived work"),
        "expected noop detail in dispatch ack, got: {ack:?}"
    );

    let discovery = client
        .get(format!("{}/agents", base_url))
        .send()
        .await
        .expect("discovery request");
    assert_eq!(discovery.status(), reqwest::StatusCode::OK);
    let agents: Value = discovery.json().await.expect("discovery json");
    assert!(
        agents
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item.pointer("/agent_package").and_then(Value::as_str)
                    == Some("workflow-intake-agent")
            })),
        "workflow-intake-agent must be discoverable: {agents:?}"
    );

    server.stop().await;
}

#[tokio::test]
async fn workflow_intake_dispatch_http_rejects_slack_task_daemon_events() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry("clickup-agent", &["clickup:create-task"])],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let registry = Arc::new(DispatchRegistry::new(
        "workflow-intake-agent",
        "default",
        "workflow-intake-agent",
        "1.0.0",
        agent,
    )) as Arc<dyn AgentRegistry>;
    let app = baml_rt_api::api_router(registry, None, None).await;
    let server = start_http_server(app, None)
        .await
        .expect("start dispatch http api");
    let base_url = server.base_url.clone();

    let client = reqwest::Client::new();
    let dispatch_url = format!("{}/agents/workflow-intake-agent/default/dispatch", base_url);
    let dispatch_body = json!({
        "routing_key": "slack:intake",
        "message_type": "task-daemon.interpretation.v1",
        "messages": [
            base_event("slack", "#agentium-eng", vec![])
        ]
    });

    let response = client
        .post(&dispatch_url)
        .json(&dispatch_body)
        .send()
        .await
        .expect("dispatch request");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ack: Value = response.json().await.expect("dispatch ack json");
    assert_eq!(ack.get("accepted").and_then(Value::as_bool), Some(false));
    let detail = ack
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        detail.contains(
            "workflow-intake-agent no longer routes Slack task-daemon interpretation events"
        ),
        "expected unsupported-source detail, got: {ack:?}"
    );
    assert!(
        handler.snapshot_calls().await.is_empty(),
        "unexpected downstream delegation for rejected Slack task-daemon dispatch"
    );

    server.stop().await;
}

#[tokio::test]
async fn workflow_intake_forwards_all_derived_tasks_without_silent_truncation() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let derived_tasks: Vec<Value> = (1..=15)
        .map(|index| {
            json!({
                "key": format!("task-{index}"),
                "title": format!("Create PM task {index}"),
                "description": format!("Carry task {index} into the downstream system."),
                "priority": "medium"
            })
        })
        .collect();

    let request = task_daemon_request(
        base_event("clickup", "ClickUp monitored list", derived_tasks),
        "workflow-intake-clickup-many-1",
        "corr-1735720000000-11",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    let calls = handler.snapshot_calls().await;

    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; texts={texts:?}; responses={responses:?}"
    );
    assert!(
        calls[0].prompt.contains("Derived tasks (15 total):"),
        "expected full derived-task count in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("15. Create PM task 15"),
        "expected the final derived task to remain in prompt, got: {}",
        calls[0].prompt
    );
}

#[tokio::test]
async fn workflow_intake_routes_clickup_created_events_to_coordinator() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![
            discovery_entry("clickup-agent", &["clickup:create-task"]),
            discovery_entry(
                "coordinator-agent",
                &["coordination:routing", "coordination:synthesis"],
            ),
        ],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        with_workflow_seed(
            base_event(
                "clickup",
                "Sprint backlog",
                vec![json!({
                    "key": "clickup-created:task-42",
                    "title": "Execute ClickUp task: Harden routing semantics",
                    "description": "This ClickUp task already exists and should be routed for execution.",
                    "priority": "high"
                })],
            ),
            json!({
                "goal": "Ship the routing fix without losing task-daemon guidance.",
                "investigation_nodes": [{
                    "key": "investigate-ci",
                    "title": "Inspect the failing CI job",
                    "goal": "Determine whether routing regressions remain.",
                    "prompt": "Read the latest workflow-intake CI failure and identify the failing branch.",
                    "when_to_run": "always",
                    "depends_on": []
                }],
                "clarification_nodes": [{
                    "key": "clarify-rollout",
                    "question": "Should this change ship behind a feature flag?",
                    "blocking": true,
                    "suggested_owner": "release-manager",
                    "depends_on": []
                }],
                "follow_up_nodes": [{
                    "kind": "decision_request",
                    "prompt": "Report whether rollout can proceed once the failure is understood.",
                    "urgency": "high"
                }]
            }),
        ),
        "workflow-intake-clickup-created-1",
        "corr-1735720000000-2",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    let calls = handler.snapshot_calls().await;

    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; texts={texts:?}; responses={responses:?}"
    );
    assert_eq!(calls[0].agent_package, "coordinator-agent");
    assert!(
        calls[0].prompt.contains(
            "Execute or route the existing work item described by this task-daemon event."
        ),
        "expected execute-work prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("clickup-created:task-42"),
        "expected ClickUp lifecycle key in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0]
            .prompt
            .contains("Workflow goal:\nShip the routing fix without losing task-daemon guidance."),
        "expected workflow goal in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("Workflow investigation nodes:")
            && calls[0].prompt.contains("Inspect the failing CI job")
            && calls[0]
                .prompt
                .contains("Read the latest workflow-intake CI failure"),
        "expected workflow investigation guidance in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("Workflow clarification nodes:")
            && calls[0]
                .prompt
                .contains("Should this change ship behind a feature flag?"),
        "expected workflow clarification guidance in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        calls[0].prompt.contains("Workflow follow-up nodes:")
            && calls[0]
                .prompt
                .contains("Report whether rollout can proceed once the failure is understood."),
        "expected workflow follow-up guidance in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        texts.iter().any(|text| {
            text.contains("Routed execute_existing_work to coordinator-agent/default.")
        }),
        "expected route summary in response, got: {texts:?}"
    );
}

#[tokio::test]
async fn workflow_intake_routes_clickup_terminal_events_to_coordinator_reconciliation() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "Sprint backlog",
            vec![json!({
                "key": "clickup-terminal:task-42:done",
                "title": "Reconcile terminal ClickUp task: Harden routing semantics",
                "description": "The monitored task is terminal and in-flight automation should reconcile.",
                "priority": "medium"
            })],
        ),
        "workflow-intake-clickup-terminal-1",
        "corr-1735720000000-3",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    let calls = handler.snapshot_calls().await;

    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; texts={texts:?}; responses={responses:?}"
    );
    assert_eq!(calls[0].agent_package, "coordinator-agent");
    assert!(
        calls[0]
            .prompt
            .contains("Reconcile terminal or missing work and stop duplicate execution."),
        "expected reconciliation prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        texts.iter().any(|text| {
            text.contains("Routed cancel_or_close_work to coordinator-agent/default.")
        }),
        "expected route summary in response, got: {texts:?}"
    );
}

#[tokio::test]
async fn workflow_intake_routes_clickup_removed_events_to_coordinator_reconciliation() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "Sprint backlog",
            vec![json!({
                "key": "clickup-removed:task-42",
                "title": "Reconcile missing ClickUp task: Harden routing semantics",
                "description": "The monitored task disappeared and in-flight automation should reconcile.",
                "priority": "medium"
            })],
        ),
        "workflow-intake-clickup-removed-1",
        "corr-1735720000000-10",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    let calls = handler.snapshot_calls().await;

    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; texts={texts:?}; responses={responses:?}"
    );
    assert_eq!(calls[0].agent_package, "coordinator-agent");
    assert!(
        calls[0]
            .prompt
            .contains("Reconcile terminal or missing work and stop duplicate execution."),
        "expected reconciliation prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        texts.iter().any(|text| {
            text.contains("Routed cancel_or_close_work to coordinator-agent/default.")
        }),
        "expected route summary in response, got: {texts:?}"
    );
}

#[tokio::test]
async fn workflow_intake_completes_noop_events_without_delegation() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event("clickup", "ClickUp monitored list", vec![]),
        "workflow-intake-noop-1",
        "corr-1735720000000-4",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    let calls = handler.snapshot_calls().await;

    assert!(
        texts
            .iter()
            .any(|text| text.contains("The event produced no derived work items.")),
        "expected noop message in response, got: {texts:?}"
    );
    assert!(
        calls.is_empty(),
        "noop routing should not delegate downstream, got: {calls:?}"
    );
}

#[tokio::test]
async fn workflow_intake_fails_invalid_task_daemon_payloads() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList { entries: vec![] });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        json!({
            "schema_version": "task-daemon.interpretation.v1",
            "event_id": "td-invalid-event",
            "project": {
                "project_key": "agent-platform"
            }
        }),
        "workflow-intake-invalid-1",
        "corr-1735720000000-5",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let calls = handler.snapshot_calls().await;

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected invalid payload to fail, got: {responses:?}"
    );
    assert!(
        calls.is_empty(),
        "invalid payload should not delegate downstream, got: {calls:?}"
    );
}

#[tokio::test]
async fn workflow_intake_fails_when_no_agent_matches_required_capability() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry("clickup-agent", &["clickup:create-task"])],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "ClickUp monitored list",
            vec![json!({
                "key": "task-2",
                "title": "Execute PM task from ClickUp event",
                "description": "No coordinator-capable agent is registered in this test.",
            })],
        ),
        "workflow-intake-no-match-1",
        "corr-1735720000000-6",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let calls = handler.snapshot_calls().await;
    let rendered = serde_json::to_string(&responses).expect("serialize responses");

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected no-match routing to fail, got: {responses:?}"
    );
    assert!(
        rendered.contains("coordination:routing"),
        "expected missing-capability details in response, got: {responses:?}"
    );
    assert!(
        calls.is_empty(),
        "no-match routing should not delegate downstream, got: {calls:?}"
    );
}

#[tokio::test]
async fn workflow_intake_reports_ambiguous_downstream_matches() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![
            discovery_entry("alpha-agent", &["coordination:routing"]),
            discovery_entry("beta-agent", &["coordination:routing"]),
        ],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "ClickUp monitored list",
            vec![json!({
                "key": "clickup-created:CU-123",
                "title": "Implement task routing",
                "description": "A new ClickUp task should route into execution.",
            })],
        ),
        "workflow-intake-ambiguous-1",
        "corr-1735720000000-7",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let calls = handler.snapshot_calls().await;
    let rendered = serde_json::to_string(&responses).expect("serialize responses");

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected ambiguous routing to fail, got: {responses:?}"
    );
    assert!(
        rendered.contains("alpha-agent/default") && rendered.contains("beta-agent/default"),
        "expected ambiguity details in response, got: {responses:?}"
    );
    assert!(
        calls.is_empty(),
        "ambiguous routing should not delegate downstream, got: {calls:?}"
    );
}

#[tokio::test]
async fn workflow_intake_routes_github_issue_events_to_coordinator() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(CapturingA2aHandler::default());
    let (_permit, agent, built_dir) =
        setup_workflow_intake_agent(agent_list, handler.clone()).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "github_issues",
            "repo/issues",
            vec![json!({
                "key": "github-issue-17",
                "title": "Investigate issue-driven workflow",
                "description": "GitHub issue events should use the generic execution path for now.",
            })],
        ),
        "workflow-intake-github-1",
        "corr-1735720000000-8",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let texts = message_texts_from_chunks(&chunks_from_responses(&responses));
    let calls = handler.snapshot_calls().await;

    assert_eq!(
        calls.len(),
        1,
        "expected one downstream delegation; texts={texts:?}; responses={responses:?}"
    );
    assert_eq!(calls[0].agent_package, "coordinator-agent");
    assert!(
        calls[0]
            .prompt
            .contains("Source: github_issues (repo/issues)"),
        "expected GitHub source details in prompt, got: {}",
        calls[0].prompt
    );
    assert!(
        texts.iter().any(|text| {
            text.contains("Routed execute_existing_work to coordinator-agent/default.")
        }),
        "expected route summary in response, got: {texts:?}"
    );
}

#[tokio::test]
async fn workflow_intake_fails_when_downstream_delegation_errors() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(FailingA2aHandler);
    let (_permit, agent, built_dir) = setup_workflow_intake_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "ClickUp monitored list",
            vec![json!({
                "key": "clickup-created:CU-456",
                "title": "Execute existing work",
                "description": "The downstream coordinator is temporarily unavailable.",
            })],
        ),
        "workflow-intake-downstream-error-1",
        "corr-1735720000000-9",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let rendered = serde_json::to_string(&responses).expect("serialize responses");

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected downstream delegation failure to fail task, got: {responses:?}"
    );
    assert!(
        rendered.contains("workflow-intake-agent failed:")
            && rendered.contains("downstream agent unavailable"),
        "expected downstream error message in response, got: {responses:?}"
    );
}

#[tokio::test]
async fn workflow_intake_fails_when_delegated_child_task_reports_failed_state() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(StreamingA2aHandler {
        chunks: vec![
            json!({
                "statusUpdate": {
                    "status": { "state": "TASK_STATE_WORKING" }
                }
            }),
            json!({
                "task": {
                    "status": {
                        "state": "TASK_STATE_FAILED",
                        "message": {
                            "parts": [{ "text": "Delegated coordinator workflow failed after streaming started." }]
                        }
                    }
                }
            }),
        ],
    });
    let (_permit, agent, built_dir) = setup_workflow_intake_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "ClickUp monitored list",
            vec![json!({
                "key": "clickup-created:CU-789",
                "title": "Execute existing work",
                "description": "The delegated child task will fail after it starts streaming.",
            })],
        ),
        "workflow-intake-downstream-failed-state-1",
        "corr-1735720000000-12",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let rendered = serde_json::to_string(&responses).expect("serialize responses");

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected delegated child failure to fail task, got: {responses:?}"
    );
    assert!(
        rendered.contains("Delegated coordinator workflow failed after streaming started."),
        "expected delegated child-task failure reason in response, got: {responses:?}"
    );
}

#[tokio::test]
async fn workflow_intake_fails_when_delegated_child_task_requires_follow_up_input() {
    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList {
        entries: vec![discovery_entry(
            "coordinator-agent",
            &["coordination:routing"],
        )],
    });
    let handler = Arc::new(StreamingA2aHandler {
        chunks: vec![json!({
            "task": {
                "status": {
                    "state": "TASK_STATE_INPUT_REQUIRED",
                    "message": {
                        "parts": [{ "text": "Need human confirmation before continuing the delegated workflow." }]
                    }
                }
            }
        })],
    });
    let (_permit, agent, built_dir) = setup_workflow_intake_agent(agent_list, handler).await;
    let _built_dir_guard = TempDirCleanup::new(built_dir);

    let request = task_daemon_request(
        base_event(
            "clickup",
            "ClickUp monitored list",
            vec![json!({
                "key": "clickup-created:CU-790",
                "title": "Execute existing work",
                "description": "The delegated child task will require more input.",
            })],
        ),
        "workflow-intake-downstream-input-required-1",
        "corr-1735720000000-13",
    );

    let responses = collect_responses(&agent, request)
        .await
        .expect("workflow-intake response");
    let chunks = chunks_from_responses(&responses);
    let rendered = serde_json::to_string(&responses).expect("serialize responses");

    assert!(
        chunks
            .iter()
            .filter_map(|chunk| task_state(chunk))
            .any(|state| state == "TASK_STATE_FAILED"),
        "expected delegated child suspension to fail task, got: {responses:?}"
    );
    assert!(
        rendered.contains("Need human confirmation before continuing the delegated workflow."),
        "expected delegated input-required reason in response, got: {responses:?}"
    );
}
