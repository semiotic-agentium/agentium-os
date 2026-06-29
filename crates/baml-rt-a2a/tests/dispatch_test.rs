// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::PathBuf, sync::Arc};

use baml_rt_a2a::A2aAgent;
use baml_rt_core::{
    AgentDispatchRequest, BamlRtError, EventSchemaVersion,
    bus::BusWithEffects,
    dispatch::AgentDispatchRoutingKey,
    ids::{ContextId, ExternalId, MessageId},
};
use baml_rt_llm_config::{LlmClientConfig, ModelBudgetOverride};
use baml_rt_provenance::ProvenanceWriter;
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
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
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
        source_kind: None,
        source_key: None,
        producer_key: None,
        metadata: None,
    }
}

fn make_dispatch_request_with_context(
    routing_key: &str,
    messages: Vec<serde_json::Value>,
    context_id: ContextId,
) -> AgentDispatchRequest {
    AgentDispatchRequest {
        routing_key: AgentDispatchRoutingKey::parse(routing_key).expect("valid routing key"),
        message_type: EventSchemaVersion::parse("host.source-records.v1")
            .expect("valid schema version"),
        messages,
        context_id: Some(context_id),
        task_id: None,
        message_id: None,
        source_kind: None,
        source_key: None,
        producer_key: None,
        metadata: None,
    }
}

fn tuned_llm_config() -> LlmClientConfig {
    let mut config = LlmClientConfig::sensible_default();
    config.compaction.defaults.item_threshold = 8;
    config.compaction.defaults.recent_tail_retention = 2;
    config.compaction.client_overrides.insert(
        "OpenRouter".to_string(),
        ModelBudgetOverride {
            context_window_tokens: Some(8192),
            trigger_ratio: Some(0.35),
            emergency_ratio: Some(0.55),
            output_reserve_tokens: Some(512),
        },
    );
    config
}

async fn setup_fixture_agent_with_store(
    name: &str,
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
) -> (A2aAgent, PathBuf) {
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
        .with_surreal_store(store)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .with_llm_client_config(Arc::new(tuned_llm_config()))
        .with_compaction_summarizer(Arc::new(
            baml_rt_provenance::FixedCompactionSummarizer::new(
                "Compacted dispatch-settlement transcript prefix.",
            ),
        ))
        .build()
        .await
        .expect("build fixture agent");

    (agent, built)
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
async fn dispatch_settlement_triggers_post_turn_compaction() {
    let store = test_surreal_store().await;
    let ctx = ContextId::new(20, 30);

    let (agent, built_dir) =
        setup_fixture_agent_with_store("dispatch-echo", Arc::clone(&store)).await;
    let _cleanup = TempDirCleanup::new(built_dir);
    let agent_id = agent.agent_id().clone();

    for i in 0..10 {
        store
            .add_event(baml_rt_provenance::ProvEvent::message_received_global(
                ctx.clone(),
                MessageId::from_external(ExternalId::new(format!("dispatch-seed-{i}"))),
                "user".into(),
                vec![format!("dispatch seed {i} {}", "PAD ".repeat(400))],
                None,
                agent_id.clone(),
                1_920_000_000_000 + i as u64,
            ))
            .await
            .expect("seed");
    }
    let request = make_dispatch_request_with_context(
        "event:intake",
        vec![json!({
            "schema_version": "host.source-records.v1",
            "source": { "source_kind": "slack", "source_key": "slack:C123", "source_label": "#test" },
            "records": []
        })],
        ctx.clone(),
    );

    let ack = agent.handle_dispatch(request).await.expect("dispatch ack");
    assert!(ack.accepted, "expected accepted=true, got: {ack:?}");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        if store
            .latest_compaction_head(&ctx, None)
            .await
            .expect("head")
            .is_some()
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "compaction head not written after dispatch settlement"
        );
        tokio::task::yield_now().await;
    }
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
