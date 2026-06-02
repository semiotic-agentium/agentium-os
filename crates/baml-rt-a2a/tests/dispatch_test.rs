// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt_a2a::A2aAgent;
use baml_rt_core::{
    AgentDispatchRequest, BamlRtError, EventSchemaVersion, bus::BusWithEffects,
    dispatch::AgentDispatchRoutingKey,
};
use baml_rt_quickjs::BamlRuntimeManager;
use serde_json::json;
use test_support::common::{
    TempDirCleanup, build_agent_package_to_temp, test_surreal_store, workspace_root,
};

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

fn make_dispatch_request(
    routing_key: &str,
    messages: Vec<serde_json::Value>,
) -> AgentDispatchRequest {
    AgentDispatchRequest {
        routing_key: AgentDispatchRoutingKey::parse(routing_key).expect("valid routing key"),
        message_type: EventSchemaVersion::parse("host.source-records.v1")
            .expect("valid schema version"),
        messages,
        context_id: None,
        task_id: None,
        message_id: None,
        metadata: None,
    }
}

#[tokio::test]
async fn dispatch_echo_accepts_matching_event() {
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let request = make_dispatch_request(
        "event:intake",
        vec![json!({
            "schema_version": "host.source-records.v1",
            "source": { "source_kind": "slack", "source_key": "slack:C123", "source_label": "#test" },
            "records": []
        })],
    );

    let ack = agent.handle_dispatch(request).await.expect("dispatch ack");
    assert!(ack.accepted, "expected accepted=true, got: {ack:?}");
    assert_eq!(
        ack.detail.as_deref(),
        Some("routing_key=event:intake messages=1"),
        "unexpected detail: {ack:?}"
    );
}

#[tokio::test]
async fn dispatch_echo_handles_empty_messages() {
    let (agent, built_dir) = setup_fixture_agent("dispatch-echo").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let request = make_dispatch_request("test:intake", vec![]);

    let ack = agent.handle_dispatch(request).await.expect("dispatch ack");
    assert!(ack.accepted, "expected accepted=true, got: {ack:?}");
    assert_eq!(
        ack.detail.as_deref(),
        Some("routing_key=test:intake messages=0"),
        "unexpected detail: {ack:?}"
    );
}

#[tokio::test]
async fn dispatch_returns_function_not_found_when_agent_lacks_on_dispatch() {
    let (agent, built_dir) = setup_fixture_agent("argument-chapman").await;
    let _cleanup = TempDirCleanup::new(built_dir);

    let request = make_dispatch_request("slack:intake", vec![json!({"test": true})]);
    let err = agent
        .handle_dispatch(request)
        .await
        .expect_err("expected FunctionNotFound");

    match err {
        BamlRtError::FunctionNotFound(name) => {
            assert_eq!(name, "onDispatch", "unexpected function name: {name}");
        }
        other => panic!("expected FunctionNotFound, got: {other:?}"),
    }
}
