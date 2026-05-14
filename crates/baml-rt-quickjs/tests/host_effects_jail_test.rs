//! Platform test for the "host-mediated effects" product claim (#393 sub-task A).
//!
//! Builds the `host-effects-jail` fixture, loads it into a real `A2aAgent`,
//! sends one chat message, and asserts that every forbidden host-side effect
//! the agent attempts (fetch / require / WebSocket / XMLHttpRequest) is
//! rejected by the QuickJS host. If a binding leak ever lands, this test
//! flips `rejected: false` for the leaked op and fails — exactly the
//! regression-protection signal the demo claim depends on.

#![recursion_limit = "256"]

use std::{fs, sync::Arc};

use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{A2aStreamChunk, A2aWireRequest, collect_a2a_stream};
use serde_json::Value;
use test_support::common::{
    build_fixture_package_to_temp, chunks_from_responses, ensure_fixture_runtime_types,
    message_visible_content_from_chunks, send_stream_request,
};

const FORBIDDEN_OPS: &[&str] = &["fetch", "require", "WebSocket", "XMLHttpRequest"];

async fn setup_host_effects_jail_agent() -> A2aAgent {
    ensure_fixture_runtime_types();
    let built = build_fixture_package_to_temp("host-effects-jail").await;
    let mut manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");
    manager
        .load_schema(built.to_str().expect("utf8 path"))
        .expect("load schema");
    let agent_code = fs::read_to_string(built.join("dist").join("index.js"))
        .expect("host-effects-jail dist/index.js");
    A2aAgent::builder()
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .build()
        .await
        .expect("build host-effects-jail agent")
}

async fn collect_stream(agent: &A2aAgent, request: Value) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await?;
    let chunks: Vec<A2aStreamChunk> = collect_a2a_stream(stream).await;
    Ok(chunks.into_iter().map(A2aStreamChunk::into_inner).collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_effects_jail_rejects_forbidden_globals() {
    let agent = setup_host_effects_jail_agent().await;

    let request = send_stream_request(
        "msg-jail-1",
        "probe",
        "corr-1-1",
        Some(baml_rt_core::ids::ContextId::new(1, 1)),
    );
    let responses = collect_stream(&agent, request).await.expect("stream");
    assert!(
        !responses.is_empty(),
        "stream returned no responses; check fixture build"
    );

    let chunks = chunks_from_responses(&responses);
    let merged = message_visible_content_from_chunks(&chunks).join("");

    assert!(
        !merged.is_empty(),
        "merged visible content is empty; raw responses: {responses:?}"
    );

    // The agent's run() returns { message: JSON.stringify(report) }. The
    // visible content carries that JSON; the working-then-final chunks may
    // each emit it, so streaming-parse the first value and discard the rest.
    let start = merged.find('{').unwrap_or_else(|| {
        panic!("no JSON object in agent response; got: {merged:?}")
    });
    let report: serde_json::Map<String, Value> = serde_json::Deserializer::from_str(&merged[start..])
        .into_iter::<Value>()
        .next()
        .unwrap_or_else(|| panic!("no JSON value in agent response; got: {merged:?}"))
        .unwrap_or_else(|e| panic!("agent report is not valid JSON ({e}): {merged:?}"))
        .as_object()
        .unwrap_or_else(|| panic!("agent report is not a JSON object; got: {merged:?}"))
        .clone();

    for op in FORBIDDEN_OPS {
        let entry = report
            .get(*op)
            .unwrap_or_else(|| panic!("report missing entry for forbidden op `{op}`: {report:?}"));
        let rejected = entry
            .get("rejected")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| panic!("`{op}`.rejected is not a bool: {entry}"));
        assert!(
            rejected,
            "`{op}` is reachable from agent JS — host boundary is leaking. detail: {}",
            entry.get("detail").and_then(Value::as_str).unwrap_or("?")
        );
    }
}
