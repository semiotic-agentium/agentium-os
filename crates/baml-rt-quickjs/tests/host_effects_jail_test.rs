// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
use baml_rt_core::{A2aStreamChunk, A2aWireRequest, bus::BusStream};
use futures_util::StreamExt;
use serde_json::Value;
use test_support::common::{
    build_fixture_package_to_temp, ensure_fixture_runtime_types, send_stream_request,
};

const FORBIDDEN_OPS: &[&str] = &[
    "fetch",
    "require",
    "WebSocket",
    "XMLHttpRequest",
    "process",
    "Deno",
];

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

/// Walks a `serde_json::Value` and returns the first `&str` leaf containing
/// `needle`. Recursive over arrays and objects; non-string leaves are skipped.
fn first_string_containing<'a>(value: &'a Value, needle: &str) -> Option<&'a str> {
    match value {
        Value::String(s) if s.contains(needle) => Some(s.as_str()),
        Value::Array(arr) => arr.iter().find_map(|v| first_string_containing(v, needle)),
        Value::Object(obj) => obj
            .values()
            .find_map(|v| first_string_containing(v, needle)),
        _ => None,
    }
}

/// Consume the stream chunk-by-chunk until a chunk carries the sentinel-wrapped
/// payload, then slice it out and return. The stream is dropped on return; we
/// never accumulate a `Vec<chunk>` or merge content across chunks.
///
/// The agent's `SessionResult.message` is emitted in a single wire `Message`,
/// so the open and close sentinels are guaranteed to land in the same chunk
/// (and the same string leaf within it). Multi-chunk reassembly is YAGNI here.
async fn await_report(mut stream: BusStream<A2aStreamChunk>) -> String {
    let mut chunks_seen = 0usize;
    while let Some(chunk) = stream.next().await {
        chunks_seen += 1;
        if let Some(text) = first_string_containing(AsRef::<Value>::as_ref(&chunk), REPORT_OPEN) {
            let after_open =
                text.find(REPORT_OPEN).expect("contains check held") + REPORT_OPEN.len();
            let close_rel = text[after_open..].find(REPORT_CLOSE).unwrap_or_else(|| {
                panic!(
                    "found {REPORT_OPEN} but no matching {REPORT_CLOSE} in same chunk; \
                     agent must emit both sentinels in one wire message"
                )
            });
            return text[after_open..after_open + close_rel].to_owned();
        }
    }
    panic!("stream ended without {REPORT_OPEN} sentinel after {chunks_seen} chunks");
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
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await
        .expect("open stream");

    let payload = await_report(stream).await;

    let report: serde_json::Map<String, Value> = serde_json::from_str::<Value>(&payload)
        .unwrap_or_else(|e| {
            panic!("payload between sentinels is not valid JSON ({e}): {payload:?}")
        })
        .as_object()
        .unwrap_or_else(|| panic!("payload between sentinels is not a JSON object: {payload:?}"))
        .clone();

    let mut leaks: Vec<String> = Vec::new();
    for op in FORBIDDEN_OPS {
        let Some(entry) = report.get(*op) else {
            leaks.push(format!(
                "`{op}`: report missing entry; report keys: {:?}",
                report.keys().collect::<Vec<_>>()
            ));
            continue;
        };
        let Some(rejected) = entry.get("rejected").and_then(Value::as_bool) else {
            leaks.push(format!("`{op}`: `rejected` field is not a bool: {entry}"));
            continue;
        };
        if !rejected {
            let detail = entry.get("detail").and_then(Value::as_str).unwrap_or("?");
            leaks.push(format!(
                "`{op}` is reachable from agent JS. detail: {detail}"
            ));
        }
    }
    assert!(
        leaks.is_empty(),
        "host boundary leaks detected:\n  {}",
        leaks.join("\n  ")
    );
}
