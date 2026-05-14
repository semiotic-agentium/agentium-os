//! Platform test for the "host-mediated effects" product claim (#393 sub-task A).
//!
//! Builds the `host-effects-jail` fixture, loads it into a real `A2aAgent`,
//! sends one chat message, and asserts that every forbidden host-side effect
//! the agent attempts (fetch / require / WebSocket / XMLHttpRequest) is
//! rejected by the QuickJS host. If a binding leak ever lands, this test
//! flips `rejected: false` for the leaked op and fails — exactly the
//! regression-protection signal the demo claim depends on.

#![recursion_limit = "256"]

use std::{
    fs,
    sync::{Arc, OnceLock},
};

use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{A2aStreamChunk, A2aWireRequest, collect_a2a_stream};
use serde_json::Value;
use test_support::common::{
    build_fixture_package_to_temp, chunks_from_responses, ensure_fixture_runtime_types,
    message_visible_content_from_chunks, send_stream_request,
};

const FORBIDDEN_OPS: &[&str] = &["fetch", "require", "WebSocket", "XMLHttpRequest"];

// Bracketing markers around the JSON payload, mirrored in the fixture's
// index.ts. Sentinel slicing keeps the parser stable even if a future stream
// frame emits a `{` ahead of the report.
const REPORT_OPEN: &str = "__HOST_EFFECTS_JAIL_BEGIN__";
const REPORT_CLOSE: &str = "__HOST_EFFECTS_JAIL_END__";

/// Process-wide gate that serializes QuickJS-backed integration tests in this
/// crate the same way `crates/baml-rt-a2a/tests/http_a2a_stream_quickjs_test.rs`
/// does. Each runtime manager + bridge is heavy; running them concurrently
/// under default nextest parallelism contends on host resources and flakes.
async fn test_gate() -> tokio::sync::OwnedSemaphorePermit {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let semaphore = GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)));
    semaphore.clone().acquire_owned().await.expect("test_gate")
}

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
        // Agent does only synchronous probing + JSON.stringify; 3s catches a
        // hang fast without flake risk.
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(3_000)))
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

fn extract_report(merged: &str) -> serde_json::Map<String, Value> {
    let open = merged
        .find(REPORT_OPEN)
        .unwrap_or_else(|| panic!("missing {REPORT_OPEN} sentinel in: {merged:?}"));
    let after_open = open + REPORT_OPEN.len();
    let close_rel = merged[after_open..]
        .find(REPORT_CLOSE)
        .unwrap_or_else(|| panic!("missing {REPORT_CLOSE} sentinel after open in: {merged:?}"));
    let json = &merged[after_open..after_open + close_rel];
    serde_json::from_str::<Value>(json)
        .unwrap_or_else(|e| panic!("payload between sentinels is not valid JSON ({e}): {json:?}"))
        .as_object()
        .unwrap_or_else(|| panic!("payload between sentinels is not a JSON object: {json:?}"))
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_effects_jail_rejects_forbidden_globals() {
    let _permit = test_gate().await;
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

    let report = extract_report(&merged);

    let mut leaks: Vec<String> = Vec::new();
    for op in FORBIDDEN_OPS {
        let Some(entry) = report.get(*op) else {
            leaks.push(format!("`{op}`: report missing entry; report keys: {:?}", report.keys().collect::<Vec<_>>()));
            continue;
        };
        let Some(rejected) = entry.get("rejected").and_then(Value::as_bool) else {
            leaks.push(format!("`{op}`: `rejected` field is not a bool: {entry}"));
            continue;
        };
        if !rejected {
            let detail = entry.get("detail").and_then(Value::as_str).unwrap_or("?");
            leaks.push(format!("`{op}` is reachable from agent JS. detail: {detail}"));
        }
    }
    assert!(
        leaks.is_empty(),
        "host boundary leaks detected:\n  {}",
        leaks.join("\n  ")
    );
}
